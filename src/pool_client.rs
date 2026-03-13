// ─── pool_client.rs ───────────────────────────────────────────────────────────
// Mining pool client. Connects to the server, receives block templates,
// mines across all local CPU cores, submits valid hashes.
//
// BUG FIX: mine() sets stop=true internally when it finds a hash (to signal
// other threads to quit). The old code then checked `if !sm.load()` before
// submitting — which was always false on success. Now we just check if result
// is Some, which is correct regardless of the stop flag state.
// ─────────────────────────────────────────────────────────────────────────────

use crate::pool_server::{ClientMsg, ServerMsg};
use crossterm::{cursor, execute, style::{Color, ResetColor, SetForegroundColor}, terminal::{self, ClearType}};
use rayon::prelude::*;
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

const HASHRATE_REPORT_SECS: u64 = 5;

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

    // Handshake
    send_msg(&writer, &ClientMsg::Hello {
        email: email.clone(),
        name:  miner_name.clone(),
    });

    // ── Shared state ─────────────────────────────────────────────────────────
    // stop_mining: set to true to kill current mining job (externally or on find)
    // external_stop: set ONLY by server Stop messages — distinguishes
    //   "server told us to stop" from "we found it ourselves"
    let stop_mining   = Arc::new(AtomicBool::new(false));
    let external_stop = Arc::new(AtomicBool::new(false));
    let hash_counter  = Arc::new(AtomicU64::new(0));
    let peek_hash     = Arc::new(Mutex::new("0".repeat(16)));
    let user_quit     = Arc::new(AtomicBool::new(false));

    {
        let uq = user_quit.clone();
        ctrlc::set_handler(move || { uq.store(true, Ordering::SeqCst); })
            .expect("ctrlc handler");
    }

    // ── UI state ──────────────────────────────────────────────────────────────
    let ui_state = Arc::new(Mutex::new(ClientUi {
        start:       Instant::now(),
        status:      "Waiting for work from server...".into(),
        blocks_won:  0,
        total_cc:    0.0,
        difficulty:  0,
        block_index: 0,
        hr_history:  VecDeque::with_capacity(12),
        connected:   true,
        server_addr: server_addr.clone(),
    }));

    // ── UI refresh thread ─────────────────────────────────────────────────────
    {
        let ui    = ui_state.clone();
        let hc    = hash_counter.clone();
        let ph    = peek_hash.clone();
        let uq    = user_quit.clone();
        let email2 = email.clone();
        let name2  = miner_name.clone();

        std::thread::spawn(move || loop {
            if uq.load(Ordering::Relaxed) { break; }
            std::thread::sleep(Duration::from_millis(150));
            let hashes = hc.load(Ordering::Relaxed);
            let peek   = ph.lock().map(|p| p.clone()).unwrap_or_default();
            if let Ok(mut st) = ui.lock() {
                st.draw(hashes, &peek, &email2, &name2);
            }
        });
    }

    let mut current_handle: Option<std::thread::JoinHandle<()>> = None;
    let mut last_report = Instant::now();

    // ── Main server message loop ──────────────────────────────────────────────
    for line in reader.lines() {
        if user_quit.load(Ordering::Relaxed) { break; }

        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }

        let msg: ServerMsg = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                if let Ok(mut st) = ui_state.lock() {
                    st.status = format!("Bad message from server: {}", e);
                }
                continue;
            }
        };

        // Periodic hashrate report
        if last_report.elapsed().as_secs() >= HASHRATE_REPORT_SECS {
            let hr   = hash_counter.load(Ordering::Relaxed);
            let secs = last_report.elapsed().as_secs_f64();
            let rate = if secs > 0.0 { (hr as f64 / secs) as u64 } else { 0 };
            send_msg(&writer, &ClientMsg::Hashrate { hr: rate });
            hash_counter.store(0, Ordering::Relaxed);
            last_report = Instant::now();
        }

        match msg {
            // ── New block to mine ─────────────────────────────────────────────
            ServerMsg::Work { index, prev_hash, difficulty, timestamp, data, reward } => {
                // Stop any current job cleanly
                external_stop.store(false, Ordering::SeqCst);
                stop_mining.store(true, Ordering::SeqCst);
                if let Some(h) = current_handle.take() {
                    let _ = h.join();
                }
                stop_mining.store(false, Ordering::SeqCst);

                if let Ok(mut st) = ui_state.lock() {
                    st.status      = format!("Mining block #{} at difficulty {} ...", index, difficulty);
                    st.difficulty  = difficulty;
                    st.block_index = index;
                }

                // Spawn new mining job
                let sm  = stop_mining.clone();
                let ext = external_stop.clone();
                let hc  = hash_counter.clone();
                let ph  = peek_hash.clone();
                let w2  = writer.clone();
                let ui2 = ui_state.clone();

                current_handle = Some(std::thread::spawn(move || {
                    let result = mine(
                        index, &prev_hash, difficulty,
                        timestamp, &data, sm, hc, ph,
                    );

                    // ── FIX: check result directly, NOT the stop flag ─────────
                    // mine() sets stop=true internally on success to halt other
                    // threads, so checking !stop here would always be false.
                    // We only skip submission if the SERVER told us to stop.
                    if let Some((nonce, hash)) = result {
                        if !ext.load(Ordering::Relaxed) {
                            send_msg(&w2, &ClientMsg::Submit { index, nonce, hash });
                            if let Ok(mut st) = ui2.lock() {
                                st.status = format!(
                                    "Submitted block #{} — waiting for confirmation...", index
                                );
                            }
                        }
                    }
                }));
            }

            // ── Server says stop (another miner found it) ─────────────────────
            ServerMsg::Stop => {
                external_stop.store(true, Ordering::SeqCst);
                stop_mining.store(true, Ordering::SeqCst);
                if let Ok(mut st) = ui_state.lock() {
                    st.status = "Another miner found the block. Waiting for new work...".into();
                }
            }

            // ── Our submission accepted ───────────────────────────────────────
            ServerMsg::Accepted { block_index, reward } => {
                if let Ok(mut st) = ui_state.lock() {
                    st.blocks_won += 1;
                    st.total_cc   += reward;
                    st.status      = format!(
                        "Block #{} ACCEPTED! Earned {:.4} CC", block_index, reward
                    );
                }
            }

            // ── Our submission rejected ───────────────────────────────────────
            ServerMsg::Rejected { reason } => {
                if let Ok(mut st) = ui_state.lock() {
                    st.status = format!("Submission rejected: {}", reason);
                }
            }
        }
    }

    // ── Disconnected ─────────────────────────────────────────────────────────
    stop_mining.store(true, Ordering::SeqCst);
    send_msg(&writer, &ClientMsg::Bye);
    if let Ok(mut st) = ui_state.lock() { st.connected = false; }

    clear();
    execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
    println!("\n  Disconnected from pool.");
    execute!(io::stdout(), ResetColor).ok();
    if let Ok(st) = ui_state.lock() {
        println!("  Blocks Won : {}", st.blocks_won);
        println!("  CC Earned  : {:.4} CC", st.total_cc);
    }
    println!();
}

// ── Mining core ───────────────────────────────────────────────────────────────

fn mine(
    index:      u64,
    prev_hash:  &str,
    difficulty: usize,
    timestamp:  u64,
    data:       &str,
    stop:       Arc<AtomicBool>,
    counter:    Arc<AtomicU64>,
    peek:       Arc<Mutex<String>>,
) -> Option<(u64, String)> {
    let prefix    = "0".repeat(difficulty);
    let n_threads = num_cpus::get().max(1);
    let chunk     = u64::MAX / n_threads as u64;
    let result    = Arc::new(Mutex::new(None::<(u64, String)>));

    // Start each thread at a random offset so pool members don't duplicate work
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
            let hash = {
                let mut h = Sha256::new();
                h.update(raw.as_bytes());
                hex::encode(h.finalize())
            };
            local += 1;

            if local % 10_000 == 0 {
                counter_r.fetch_add(10_000, Ordering::Relaxed);
                if let Ok(mut p) = peek_r.try_lock() { *p = hash[..16].to_string(); }
                local = 0;
            }

            if hash.starts_with(prefix_r.as_str()) {
                *result_r.lock().unwrap() = Some((nonce, hash));
                stop_r.store(true, Ordering::Relaxed); // signal other threads to stop
                return;
            }

            nonce = nonce.wrapping_add(1);
        }
    });

    Arc::try_unwrap(result).ok()?.into_inner().ok()?
}

// ── Client terminal UI ────────────────────────────────────────────────────────

struct ClientUi {
    start:       Instant,
    status:      String,
    blocks_won:  u64,
    total_cc:    f64,
    difficulty:  usize,
    block_index: u64,
    hr_history:  VecDeque<(Instant, u64)>,
    connected:   bool,
    server_addr: String,
}

impl ClientUi {
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

    fn draw(&mut self, hashes: u64, peek: &str, email: &str, name: &str) {
        let mut out = io::stdout();
        let rhr     = self.recent_hr(hashes);
        let up      = self.start.elapsed().as_secs();
        let d       = self.difficulty;

        execute!(out, cursor::MoveTo(0, 0)).ok();

        execute!(out, SetForegroundColor(Color::Yellow)).ok();
        writeln!(out, "+------------------------------------------------------------------+").ok();
        writeln!(out, "|    CRYPTOCRAFT  Pool Client                                      |").ok();
        writeln!(out, "+------------------------------------------------------------------+").ok();
        execute!(out, ResetColor).ok();

        execute!(out, SetForegroundColor(Color::Cyan)).ok();
        writeln!(out, "  Account : {:<28}  Uptime : {}", email, fmt_up(up)).ok();
        writeln!(out, "  Miner   : {:<28}  Server : {}", name, self.server_addr).ok();
        execute!(out, SetForegroundColor(
            if self.connected { Color::Green } else { Color::Red }
        )).ok();
        writeln!(out, "  Pool    : {}",
            if self.connected { "CONNECTED" } else { "DISCONNECTED" }).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::Green)).ok();
        writeln!(out, "  Blocks Won    : {:<10}  CC Earned  : {:.4} CC",
            self.blocks_won, self.total_cc).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::Magenta)).ok();
        writeln!(out, "  Current Job   : Block #{:<10}  Difficulty : {} zeros",
            self.block_index, d).ok();
        execute!(out, ResetColor).ok();

        execute!(out, SetForegroundColor(Color::Yellow)).ok();
        writeln!(out, "  Hashrate      : {:<20}  Hashes : {}", fmt_hr(rhr), hashes).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        let target = format!(
            "{}{}",
            "0".repeat(d.min(16)),
            "x".repeat(16usize.saturating_sub(d))
        );
        execute!(out, SetForegroundColor(Color::Cyan)).ok();
        writeln!(out, "  Target        : {}...", target).ok();

        let hit = peek.len().min(d);
        write!(out, "  Current Peek  : ").ok();
        execute!(out, SetForegroundColor(Color::Green)).ok();
        write!(out, "{}", &peek[..hit.min(peek.len())]).ok();
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "{:<18}",
            if hit < peek.len() { &peek[hit..] } else { "" }).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::White)).ok();
        writeln!(out, "  Status        : {:<58}", &self.status).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "  [Ctrl+C] Disconnect from pool                                    ").ok();
        execute!(out, ResetColor).ok();

        out.flush().ok();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn send_msg(writer: &Arc<Mutex<TcpStream>>, msg: &ClientMsg) {
    if let Ok(mut w) = writer.lock() {
        let mut line = serde_json::to_string(msg).unwrap_or_default();
        line.push('\n');
        let _ = w.write_all(line.as_bytes());
    }
}

fn fmt_hr(hr: u64) -> String {
    if hr >= 1_000_000 { format!("{:.2} MH/s", hr as f64 / 1_000_000.0) }
    else if hr >= 1_000 { format!("{:.2} KH/s", hr as f64 / 1_000.0) }
    else                { format!("{} H/s", hr) }
}

fn fmt_up(s: u64) -> String {
    if s < 60      { format!("{}s", s) }
    else if s < 3600 { format!("{}m {}s", s/60, s%60) }
    else           { format!("{}h {}m", s/3600, (s%3600)/60) }
}

fn clear() {
    execute!(io::stdout(), terminal::Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
}
