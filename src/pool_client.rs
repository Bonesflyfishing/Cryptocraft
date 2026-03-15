// ─── pool_client.rs ───────────────────────────────────────────────────────────
// Mining pool client + HTTP dashboard server on port 2700.
//
// HTTP port 2700 → dashboard_pool_client.html + /client_stats JSON
// TCP            → connects to pool server for mining work
// ─────────────────────────────────────────────────────────────────────────────

use crate::pool_server::{ClientMsg, ServerMsg};
use crossterm::{cursor, execute, style::{Color, ResetColor, SetForegroundColor}, terminal::{self, ClearType}};
use rayon::prelude::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader, Write},
    net::TcpStream,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tiny_http::{Header, Response, Server};

const HASHRATE_REPORT_SECS: u64 = 5;
const DASHBOARD_PORT: u16       = 2700;

// ── Shared client stats (read by HTTP server, written by mining loop) ─────────

#[derive(Serialize, Clone)]
struct WinRecord {
    block_index: u64,
    hash:        String,
    reward:      f64,
    timestamp:   u64,
}

#[derive(Serialize, Clone)]
pub struct ClientStats {
    pub miner_name:   String,
    pub hashrate:     u64,
    pub blocks_won:   u64,
    pub total_cc:     f64,
    pub connected:    bool,
    pub server_addr:  String,
    pub current_block: u64,
    pub difficulty:   usize,
    pub status:       String,
    pub peek_hash:    String,
    pub wins_history: Vec<WinRecord>,
}

impl ClientStats {
    fn new(miner_name: String, server_addr: String) -> Self {
        ClientStats {
            miner_name, server_addr,
            hashrate: 0, blocks_won: 0, total_cc: 0.0,
            connected: false, current_block: 0, difficulty: 0,
            status: "Connecting...".into(),
            peek_hash: "0".repeat(16),
            wins_history: Vec::new(),
        }
    }
}

// ── Client entry point ────────────────────────────────────────────────────────

pub fn run(email: String, miner_name: String, server_addr: String) {
    clear();
    println!();
    execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
    println!("  Connecting to pool at {} ...", server_addr);
    execute!(io::stdout(), ResetColor).ok();

    let stream = loop {
        match TcpStream::connect(&server_addr) {
            Ok(s) => {
                execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
                println!("  Connected!");
                execute!(io::stdout(), ResetColor).ok();
                std::thread::sleep(Duration::from_millis(400));
                break s;
            }
            Err(e) => {
                execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
                println!("  Could not connect ({}). Retrying in 3s...", e);
                execute!(io::stdout(), ResetColor).ok();
                std::thread::sleep(Duration::from_secs(3));
            }
        }
    };

    stream.set_nodelay(true).ok();

    let writer = Arc::new(Mutex::new(stream.try_clone().expect("clone stream")));
    let reader = BufReader::new(stream);

    // Shared stats — written by mining loop, read by HTTP server
    let shared_stats = Arc::new(Mutex::new(
        ClientStats::new(miner_name.clone(), server_addr.clone())
    ));

    // Send Hello
    send_msg(&writer, &ClientMsg::Hello { email: email.clone(), name: miner_name.clone() });
    if let Ok(mut st) = shared_stats.lock() { st.connected = true; st.status = "Waiting for work...".into(); }

    // ── HTTP dashboard server ─────────────────────────────────────────────────
    // Serves the dashboard and proxies /pool_stats from the pool server so
    // the dashboard sees all miners, not just this client.
    {
        let ss          = shared_stats.clone();
        let html        = include_str!("../dashboard.html").to_string();
        // server_addr is "192.168.x.x:8080" — swap port to 2700 for HTTP
        let pool_dashboard_addr = server_addr
            .split(':').next()
            .map(|ip| format!("{}:2700", ip))
            .unwrap_or_else(|| server_addr.clone());

        std::thread::spawn(move || {
            let addr   = format!("0.0.0.0:{}", DASHBOARD_PORT);
            let server = match Server::http(&addr) {
                Ok(s) => s,
                Err(e) => { eprintln!("[client dashboard] bind error: {}", e); return; }
            };
            let html_bytes = Arc::new(html.into_bytes());

            for req in server.incoming_requests() {
                let path = req.url().split('?').next().unwrap_or("/").to_string();
                match path.as_str() {
                    "/" | "/index.html" => {
                        let body = html_bytes.as_ref().to_vec();
                        let _ = req.respond(Response::from_data(body)
                            .with_header(ct("text/html; charset=utf-8"))
                            .with_header(no_cache()));
                    }

                    // Proxy /pool_stats from the pool server
                    "/pool_stats" => {
                        let pool_url = format!("http://{}/pool_stats", pool_dashboard_addr);
                        let json = fetch_url(&pool_url)
                            .unwrap_or_else(|| r#"{"error":"pool server unreachable"}"#.into());
                        let _ = req.respond(Response::from_data(json.into_bytes())
                            .with_header(ct("application/json"))
                            .with_header(cors())
                            .with_header(no_cache()));
                    }

                    "/status" => {
                        // Include this client's own stats alongside the mode
                        let connected = ss.lock()
                            .map(|s| s.connected).unwrap_or(false);
                        let body = format!(
                            r#"{{"mode":"pool_client","connected":{}}}"#,
                            connected
                        );
                        let _ = req.respond(Response::from_data(body.into_bytes())
                            .with_header(ct("application/json"))
                            .with_header(cors()));
                    }

                    _ => { let _ = req.respond(Response::from_data(b"404".to_vec()).with_status_code(404)); }
                }
            }
        });
    }

    // ── Atomic mining state ───────────────────────────────────────────────────
    let stop_mining   = Arc::new(AtomicBool::new(false));
    let external_stop = Arc::new(AtomicBool::new(false));
    let hash_counter  = Arc::new(AtomicU64::new(0));
    let peek_hash     = Arc::new(Mutex::new("0".repeat(16)));
    let user_quit     = Arc::new(AtomicBool::new(false));

    // ── UI thread — also handles Q keypress ──────────────────────────────────
    {
        let ss  = shared_stats.clone();
        let hc  = hash_counter.clone();
        let ph  = peek_hash.clone();
        let uq  = user_quit.clone();
        let sm  = stop_mining.clone();
        let mut ui = ClientTermUi::new();

        std::thread::spawn(move || {
            let _ = crossterm::terminal::enable_raw_mode();
            loop {
                if uq.load(Ordering::Relaxed) {
                    let _ = crossterm::terminal::disable_raw_mode();
                    break;
                }

                // Poll for Q keypress
                if crossterm::event::poll(Duration::from_millis(0)).unwrap_or(false) {
                    if let Ok(crossterm::event::Event::Key(key)) = crossterm::event::read() {
                        if key.kind == crossterm::event::KeyEventKind::Press
                            && (key.code == crossterm::event::KeyCode::Char('q')
                             || key.code == crossterm::event::KeyCode::Char('Q'))
                        {
                            let _ = crossterm::terminal::disable_raw_mode();
                            sm.store(true, Ordering::SeqCst);
                            uq.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                }

                std::thread::sleep(Duration::from_millis(150));
                let hashes = hc.load(Ordering::Relaxed);
                let peek   = ph.lock().map(|p| p.clone()).unwrap_or_default();

                if let Ok(mut st) = ss.lock() {
                    let rhr  = ui.recent_hr(hashes);
                    st.hashrate  = rhr;
                    st.peek_hash = peek.clone();
                }

                if let Ok(st) = ss.lock() {
                    ui.draw(&st, hashes, &peek);
                }
            }
        });
    }

    let mut current_handle: Option<std::thread::JoinHandle<()>> = None;

    // ── Dedicated hashrate reporter thread ────────────────────────────────────
    // Runs independently every 5s so the server always sees live hashrate,
    // even between blocks when the message loop is blocked waiting for work.
    {
        let hc = hash_counter.clone();
        let w  = writer.clone();
        let uq = user_quit.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(HASHRATE_REPORT_SECS));
                if uq.load(Ordering::Relaxed) { break; }
                let hr   = hc.load(Ordering::Relaxed);
                let rate = (hr as f64 / HASHRATE_REPORT_SECS as f64) as u64;
                send_msg(&w, &ClientMsg::Hashrate { hr: rate });
                hc.store(0, Ordering::Relaxed);
            }
        });
    }

    // ── Main message loop ─────────────────────────────────────────────────────
    for line in reader.lines() {
        if user_quit.load(Ordering::Relaxed) { break; }

        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }

        let msg: ServerMsg = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                if let Ok(mut st) = shared_stats.lock() { st.status = format!("Bad msg: {}", e); }
                continue;
            }
        };

        match msg {
            ServerMsg::Work { index, prev_hash, difficulty, timestamp, data, reward: _ } => {
                external_stop.store(false, Ordering::SeqCst);
                stop_mining.store(true, Ordering::SeqCst);
                if let Some(h) = current_handle.take() { let _ = h.join(); }
                stop_mining.store(false, Ordering::SeqCst);

                if let Ok(mut st) = shared_stats.lock() {
                    st.status        = format!("Mining block #{} at {} zeros", index, difficulty);
                    st.difficulty    = difficulty;
                    st.current_block = index;
                }

                let sm  = stop_mining.clone();
                let ext = external_stop.clone();
                let hc  = hash_counter.clone();
                let ph  = peek_hash.clone();
                let w2  = writer.clone();
                let ss2 = shared_stats.clone();

                current_handle = Some(std::thread::spawn(move || {
                    let result = mine(index, &prev_hash, difficulty, timestamp, &data, sm, hc, ph);
                    if let Some((nonce, hash)) = result {
                        if !ext.load(Ordering::Relaxed) {
                            send_msg(&w2, &ClientMsg::Submit { index, nonce, hash });
                            if let Ok(mut st) = ss2.lock() {
                                st.status = format!("Submitted block #{} — waiting for confirmation...", index);
                            }
                        }
                    }
                }));
            }

            ServerMsg::Stop => {
                external_stop.store(true, Ordering::SeqCst);
                stop_mining.store(true, Ordering::SeqCst);
                if let Ok(mut st) = shared_stats.lock() {
                    st.status = "Another miner found the block. Waiting for new work...".into();
                }
            }

            ServerMsg::Accepted { block_index, reward } => {
                if let Ok(mut st) = shared_stats.lock() {
                    st.blocks_won += 1;
                    st.total_cc   += reward;
                    st.status      = format!("Block #{} ACCEPTED! +{:.4} CC", block_index, reward);
                    let hash = st.peek_hash.clone();
                    st.wins_history.push(WinRecord {
                        block_index,
                        hash,
                        reward,
                        timestamp: crate::blockchain::now_secs(),
                    });
                    if st.wins_history.len() > 50 { st.wins_history.remove(0); }
                }
            }

            ServerMsg::Rejected { reason } => {
                if let Ok(mut st) = shared_stats.lock() {
                    st.status = format!("Submission rejected: {}", reason);
                }
            }
        }
    }

    // Disconnected
    stop_mining.store(true, Ordering::SeqCst);
    send_msg(&writer, &ClientMsg::Bye);
    if let Ok(mut st) = shared_stats.lock() {
        st.connected = false;
        st.status    = "Disconnected from pool.".into();
    }

    clear();
    execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
    println!("\n  Disconnected from pool.");
    execute!(io::stdout(), ResetColor).ok();
    if let Ok(st) = shared_stats.lock() {
        println!("  Blocks Won : {}", st.blocks_won);
        println!("  CC Earned  : {:.4} CC", st.total_cc);
    }
    println!();
}

// ── Mining core ───────────────────────────────────────────────────────────────

fn mine(
    index: u64, prev_hash: &str, difficulty: usize,
    timestamp: u64, data: &str,
    stop: Arc<AtomicBool>, counter: Arc<AtomicU64>, peek: Arc<Mutex<String>>,
) -> Option<(u64, String)> {
    let prefix    = "0".repeat(difficulty);
    let n_threads = num_cpus::get().max(1);
    let chunk     = u64::MAX / n_threads as u64;
    let result    = Arc::new(Mutex::new(None::<(u64, String)>));
    let base: u64 = rand::random();

    (0..n_threads).into_par_iter().for_each(|t| {
        let start     = base.wrapping_add(t as u64 * chunk);
        let stop_r    = stop.clone();
        let counter_r = counter.clone();
        let peek_r    = peek.clone();
        let result_r  = result.clone();
        let prefix_r  = prefix.clone();
        let mut local = 0u64;
        let mut nonce = start;

        loop {
            if stop_r.load(Ordering::Relaxed) { return; }
            if result_r.lock().unwrap().is_some() { return; }
            let raw  = format!("{}{}{}{}{}{}", index, timestamp, data, prev_hash, nonce, difficulty);
            let hash = { let mut h = Sha256::new(); h.update(raw.as_bytes()); hex::encode(h.finalize()) };
            local   += 1;
            if local % 10_000 == 0 {
                counter_r.fetch_add(10_000, Ordering::Relaxed);
                if let Ok(mut p) = peek_r.try_lock() { *p = hash[..16].to_string(); }
                local = 0;
            }
            if hash.starts_with(prefix_r.as_str()) {
                *result_r.lock().unwrap() = Some((nonce, hash));
                stop_r.store(true, Ordering::Relaxed);
                return;
            }
            nonce = nonce.wrapping_add(1);
        }
    });

    Arc::try_unwrap(result).ok()?.into_inner().ok()?
}

// ── Terminal UI ───────────────────────────────────────────────────────────────

struct ClientTermUi {
    start:      Instant,
    hr_history: VecDeque<(Instant, u64)>,
}

impl ClientTermUi {
    fn new() -> Self { ClientTermUi { start: Instant::now(), hr_history: VecDeque::with_capacity(12) } }

    fn recent_hr(&mut self, hashes: u64) -> u64 {
        let now = Instant::now();
        self.hr_history.push_back((now, hashes));
        if self.hr_history.len() > 12 { self.hr_history.pop_front(); }
        if self.hr_history.len() < 2  { return 0; }
        let (t0, h0) = self.hr_history.front().unwrap();
        let (t1, h1) = self.hr_history.back().unwrap();
        let dt = t1.duration_since(*t0).as_secs_f64();
        if dt > 0.0 { ((h1 - h0) as f64 / dt) as u64 } else { 0 }
    }

    fn draw(&self, st: &ClientStats, hashes: u64, peek: &str) {
        let mut out = io::stdout();
        let up      = self.start.elapsed().as_secs();
        let d       = st.difficulty;

        execute!(out, cursor::MoveTo(0, 0)).ok();
        execute!(out, SetForegroundColor(Color::Yellow)).ok();
        writeln!(out, "+------------------------------------------------------------------+").ok();
        writeln!(out, "|    CRYPTOCRAFT  Pool Client                                      |").ok();
        writeln!(out, "+------------------------------------------------------------------+").ok();
        execute!(out, ResetColor).ok();

        execute!(out, SetForegroundColor(Color::Cyan)).ok();
        writeln!(out, "  Miner   : {:<28}  Uptime : {}", st.miner_name, fmt_up(up)).ok();
        writeln!(out, "  Server  : {:<28}  Port   : {}", st.server_addr, DASHBOARD_PORT).ok();
        execute!(out, SetForegroundColor(if st.connected { Color::Green } else { Color::Red })).ok();
        writeln!(out, "  Pool    : {}  Dashboard: http://localhost:{}/",
            if st.connected { "CONNECTED  " } else { "DISCONNECTED" }, DASHBOARD_PORT).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::Green)).ok();
        writeln!(out, "  Blocks Won    : {:<10}  CC Earned  : {:.4} CC", st.blocks_won, st.total_cc).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::Magenta)).ok();
        writeln!(out, "  Current Job   : Block #{:<10}  Difficulty : {} zeros", st.current_block, d).ok();
        execute!(out, ResetColor).ok();
        execute!(out, SetForegroundColor(Color::Yellow)).ok();
        writeln!(out, "  Hashrate      : {:<20}  Hashes : {}", fmt_hr(st.hashrate), hashes).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        let target = format!("{}{}", "0".repeat(d.min(16)), "x".repeat(16usize.saturating_sub(d)));
        execute!(out, SetForegroundColor(Color::Cyan)).ok();
        writeln!(out, "  Target        : {}...", target).ok();
        let hit = peek.len().min(d);
        write!(out, "  Current Peek  : ").ok();
        execute!(out, SetForegroundColor(Color::Green)).ok();
        write!(out, "{}", &peek[..hit.min(peek.len())]).ok();
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "{:<18}", if hit < peek.len() { &peek[hit..] } else { "" }).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::White)).ok();
        writeln!(out, "  Status        : {:<58}", &st.status).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "  [Q + Enter] Back to menu                                         ").ok();
        execute!(out, ResetColor).ok();
        out.flush().ok();
    }
}

// ── HTTP header helpers ───────────────────────────────────────────────────────

fn ct(s: &str) -> Header { Header::from_bytes("Content-Type", s).unwrap() }
fn cors()      -> Header { Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap() }
fn no_cache()  -> Header { Header::from_bytes("Cache-Control", "no-cache, no-store").unwrap() }

/// Simple blocking HTTP GET — used to proxy /pool_stats from the pool server.
/// Returns the response body as a String, or None on failure.
fn fetch_url(url: &str) -> Option<String> {
    // Parse host and path from URL
    let without_scheme = url.strip_prefix("http://")?;
    let (host, path)   = if let Some(pos) = without_scheme.find('/') {
        (&without_scheme[..pos], &without_scheme[pos..])
    } else {
        (without_scheme, "/")
    };

    let mut stream = TcpStream::connect(host).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();

    let request = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, host
    );
    stream.write_all(request.as_bytes()).ok()?;

    let mut response = String::new();
    use std::io::Read;
    stream.read_to_string(&mut response).ok()?;

    // Strip HTTP headers — body starts after \r\n\r\n
    response.find("\r\n\r\n")
        .map(|pos| response[pos + 4..].to_string())
}

// ── Misc helpers ─────────────────────────────────────────────────────────────

fn send_msg(writer: &Arc<Mutex<TcpStream>>, msg: &ClientMsg) {
    if let Ok(mut w) = writer.lock() {
        let mut line = serde_json::to_string(msg).unwrap_or_default();
        line.push('\n');
        let _ = w.write_all(line.as_bytes());
    }
}

fn fmt_hr(hr: u64) -> String {
    if hr >= 1_000_000 { format!("{:.2} MH/s", hr as f64/1_000_000.0) }
    else if hr >= 1_000 { format!("{:.2} KH/s", hr as f64/1_000.0) }
    else { format!("{} H/s", hr) }
}

fn fmt_up(s: u64) -> String {
    if s < 60 { format!("{}s", s) } else if s < 3600 { format!("{}m {}s", s/60, s%60) } else { format!("{}h {}m", s/3600, (s%3600)/60) }
}

fn clear() {
    execute!(io::stdout(), terminal::Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
}
