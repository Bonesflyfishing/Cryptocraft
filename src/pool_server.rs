// ─── pool_server.rs ───────────────────────────────────────────────────────────
// TCP mining pool server + HTTP dashboard server on port 2700.
//
// TCP port 8080  → mining protocol (work distribution, submissions)
// UDP port 8081  → discovery broadcast responder
// HTTP port 2700 → dashboard_pool_server.html + /pool_stats JSON
// ─────────────────────────────────────────────────────────────────────────────

use crate::blockchain::Blockchain;
use crossterm::{cursor, execute, style::{Color, ResetColor, SetForegroundColor}};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tiny_http::{Header, Response, Server};

pub const POOL_PORT: u16       = 8080;
pub const DISCOVERY_PORT: u16  = 8081;
pub const DASHBOARD_PORT: u16  = 2700;
pub const DISCOVERY_PING: &str = "CRYPTOCRAFT_DISCOVER_V1";
pub const DISCOVERY_PONG: &str = "CRYPTOCRAFT_POOL_V1";

// ── Wire protocol ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Work     { index: u64, prev_hash: String, difficulty: usize, timestamp: u64, data: String, reward: f64 },
    Stop,
    Accepted { block_index: u64, reward: f64 },
    Rejected { reason: String },
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello    { email: String, name: String },
    Submit   { index: u64, nonce: u64, hash: String },
    Hashrate { hr: u64 },
    Bye,
}

// ── Per-miner state ───────────────────────────────────────────────────────────

struct MinerConn {
    email:        String,
    name:         String,
    hashrate:     u64,
    blocks_found: u64,
    joined:       Instant,
    writer:       Arc<Mutex<TcpStream>>,
}

impl MinerConn {
    fn send(&self, msg: &ServerMsg) -> bool {
        if let Ok(mut w) = self.writer.lock() {
            let mut line = serde_json::to_string(msg).unwrap_or_default();
            line.push('\n');
            w.write_all(line.as_bytes()).is_ok()
        } else { false }
    }
}

// ── Pool stats (serializable snapshot for the dashboard) ─────────────────────

#[derive(Serialize)]
struct MinerStats {
    name:         String,
    hashrate:     u64,
    blocks_found: u64,
    uptime_secs:  u64,
}

#[derive(Serialize)]
struct RecentBlock {
    index:      u64,
    hash:       String,
    difficulty: usize,
    miner:      String,
    reward:     f64,
    timestamp:  u64,
}

#[derive(Serialize)]
struct PoolStats {
    chain_length:    usize,
    difficulty:      usize,
    total_mined:     f64,
    blocks_total:    u64,
    total_hashrate:  u64,
    miners:          Vec<MinerStats>,
    recent_blocks:   Vec<RecentBlock>,
}

// ── Shared pool state ─────────────────────────────────────────────────────────

pub struct PoolState {
    pub blockchain:   Blockchain,
    pub save_file:    String,
    pub miners:       HashMap<String, MinerConn>,
    pub blocks_total: u64,
    pub bind_ip:      String,
}

impl PoolState {
    fn build_work(&self) -> ServerMsg {
        let bc = &self.blockchain;
        ServerMsg::Work {
            index:      bc.next_index(),
            prev_hash:  bc.latest_hash().to_string(),
            difficulty: bc.difficulty,
            timestamp:  crate::blockchain::now_secs(),
            data:       format!("CryptoCraft Pool Block #{}", bc.next_index()),
            reward:     bc.current_reward(),
        }
    }

    fn total_hashrate(&self) -> u64 {
        self.miners.values().map(|m| m.hashrate).sum()
    }

    fn to_stats(&self) -> PoolStats {
        let miners = self.miners.values().map(|m| MinerStats {
            name:         m.name.clone(),
            hashrate:     m.hashrate,
            blocks_found: m.blocks_found,
            uptime_secs:  m.joined.elapsed().as_secs(),
        }).collect();

        let recent_blocks = self.blockchain.chain.iter().rev()
            .filter(|b| b.index > 0)
            .take(20)
            .map(|b| RecentBlock {
                index:      b.index,
                hash:       b.hash.clone(),
                difficulty: b.difficulty,
                miner:      b.miner.clone(),
                reward:     b.reward,
                timestamp:  b.timestamp,
            })
            .collect::<Vec<_>>()
            .into_iter().rev().collect();

        PoolStats {
            chain_length:   self.blockchain.chain.len(),
            difficulty:     self.blockchain.difficulty,
            total_mined:    self.blockchain.total_mined,
            blocks_total:   self.blocks_total,
            total_hashrate: self.total_hashrate(),
            miners,
            recent_blocks,
        }
    }
}

// ── Server entry point ────────────────────────────────────────────────────────

pub fn run(blockchain: Blockchain, save_file: String, bind_ip: String) {
    use std::sync::atomic::{AtomicBool, Ordering};

    let quit = Arc::new(AtomicBool::new(false));

    // Q + Enter to return to menu
    {
        let q = quit.clone();
        std::thread::spawn(move || {
            let stdin  = std::io::stdin();
            let locked = stdin.lock();
            for line in locked.lines() {
                if let Ok(l) = line {
                    if l.trim().eq_ignore_ascii_case("q") {
                        q.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });
    }

    let state = Arc::new(Mutex::new(PoolState {
        blockchain,
        save_file,
        miners: HashMap::new(),
        blocks_total: 0,
        bind_ip: bind_ip.clone(),
    }));

    // ── HTTP dashboard server on port 2700 ────────────────────────────────────
    {
        let s    = state.clone();
        let html = include_str!("../dashboard.html").to_string();
        std::thread::spawn(move || {
            let addr = format!("0.0.0.0:{}", DASHBOARD_PORT);
            let server = match Server::http(&addr) {
                Ok(s) => s,
                Err(e) => { eprintln!("[pool dashboard] bind error: {}", e); return; }
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
                    "/pool_stats" => {
                        let json = if let Ok(st) = s.lock() {
                            serde_json::to_string(&st.to_stats()).unwrap_or_else(|_| "{}".into())
                        } else { "{}".into() };
                        let _ = req.respond(Response::from_data(json.into_bytes())
                            .with_header(ct("application/json"))
                            .with_header(cors())
                            .with_header(no_cache()));
                    }
                    "/status" => {
                        let body = r#"{"mode":"pool_server","mining":true}"#;
                        let _ = req.respond(Response::from_data(body.as_bytes().to_vec())
                            .with_header(ct("application/json"))
                            .with_header(cors()));
                    }
                    _ => { let _ = req.respond(Response::from_data(b"404".to_vec()).with_status_code(404)); }
                }
            }
        });
    }

    // ── UI refresh thread ─────────────────────────────────────────────────────
    {
        let s = state.clone();
        let q = quit.clone();
        std::thread::spawn(move || loop {
            if q.load(Ordering::Relaxed) { break; }
            std::thread::sleep(Duration::from_millis(500));
            if let Ok(st) = s.lock() { draw_server_ui(&st); }
        });
    }

    // ── UDP discovery responder ───────────────────────────────────────────────
    {
        let ip = bind_ip.clone();
        let q  = quit.clone();
        std::thread::spawn(move || {
            let sock = match UdpSocket::bind(format!("0.0.0.0:{}", DISCOVERY_PORT)) {
                Ok(s) => s, Err(_) => return,
            };
            let _ = sock.set_read_timeout(Some(Duration::from_secs(1)));
            let mut buf = [0u8; 64];
            loop {
                if q.load(Ordering::Relaxed) { break; }
                if let Ok((len, src)) = sock.recv_from(&mut buf) {
                    let msg = std::str::from_utf8(&buf[..len]).unwrap_or("");
                    if msg.trim() == DISCOVERY_PING {
                        let reply = format!("{}|{}:{}", DISCOVERY_PONG, ip, POOL_PORT);
                        let _ = sock.send_to(reply.as_bytes(), src);
                    }
                }
            }
        });
    }

    // ── TCP accept loop ───────────────────────────────────────────────────────
    let addr     = format!("{}:{}", bind_ip, POOL_PORT);
    let listener = TcpListener::bind(&addr).expect("Failed to bind pool server");
    listener.set_nonblocking(true).expect("set_nonblocking");

    loop {
        if quit.load(Ordering::Relaxed) {
            if let Ok(st) = state.lock() { st.blockchain.save(&st.save_file); }
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let s2 = state.clone();
                std::thread::spawn(move || handle_client(stream, s2));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {}
        }
    }
}

// ── Client handler ────────────────────────────────────────────────────────────

fn handle_client(stream: TcpStream, state: Arc<Mutex<PoolState>>) {
    let addr   = stream.peer_addr().map(|a| a.to_string()).unwrap_or("?".into());
    let writer = Arc::new(Mutex::new(stream.try_clone().expect("clone")));
    let reader = BufReader::new(stream);
    let mut authed = false;

    let send_work = |w: &Arc<Mutex<TcpStream>>, st: &Arc<Mutex<PoolState>>| {
        if let Ok(st) = st.lock() {
            let line = serde_json::to_string(&st.build_work()).unwrap_or_default() + "\n";
            if let Ok(mut w) = w.lock() { let _ = w.write_all(line.as_bytes()); }
        }
    };

    for line in reader.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let msg: ClientMsg = match serde_json::from_str(&line) { Ok(m) => m, Err(_) => continue };

        match msg {
            ClientMsg::Hello { email: _, name } => {
                authed = true;
                let conn = MinerConn {
                    email:        String::new(),
                    name,
                    hashrate:     0,
                    blocks_found: 0,
                    joined:       Instant::now(),
                    writer:       writer.clone(),
                };
                if let Ok(mut st) = state.lock() { st.miners.insert(addr.clone(), conn); }
                send_work(&writer, &state);
            }

            ClientMsg::Submit { index, nonce, hash } => {
                if !authed { continue; }
                let mut st = match state.lock() { Ok(s) => s, Err(_) => continue };
                let expected = st.blockchain.next_index();
                let prefix   = "0".repeat(st.blockchain.difficulty);

                if index != expected {
                    let msg = ServerMsg::Rejected { reason: "stale block".into() };
                    if let Ok(mut w) = writer.lock() { let _ = w.write_all((serde_json::to_string(&msg).unwrap()+"\n").as_bytes()); }
                    continue;
                }
                if !hash.starts_with(&prefix) {
                    let msg = ServerMsg::Rejected { reason: "invalid hash".into() };
                    if let Ok(mut w) = writer.lock() { let _ = w.write_all((serde_json::to_string(&msg).unwrap()+"\n").as_bytes()); }
                    continue;
                }

                let block = st.blockchain.add_block(nonce, hash, 0);
                st.blockchain.save(&st.save_file);
                st.blocks_total += 1;
                if let Some(conn) = st.miners.get_mut(&addr) { conn.blocks_found += 1; }

                let accept = ServerMsg::Accepted { block_index: block.index, reward: block.reward };
                if let Ok(mut w) = writer.lock() { let _ = w.write_all((serde_json::to_string(&accept).unwrap()+"\n").as_bytes()); }

                let stop = ServerMsg::Stop;
                let work = st.build_work();
                for (a, conn) in &st.miners {
                    if a != &addr { conn.send(&stop); }
                    conn.send(&work);
                }
            }

            ClientMsg::Hashrate { hr } => {
                if let Ok(mut st) = state.lock() {
                    if let Some(conn) = st.miners.get_mut(&addr) { conn.hashrate = hr; }
                }
            }

            ClientMsg::Bye => break,
            }
    }

    if let Ok(mut st) = state.lock() { st.miners.remove(&addr); }
}

// ── Terminal UI ───────────────────────────────────────────────────────────────

fn draw_server_ui(st: &PoolState) {
    let mut out = io::stdout();
    execute!(out, cursor::MoveTo(0, 0)).ok();

    execute!(out, SetForegroundColor(Color::Yellow)).ok();
    writeln!(out, "+------------------------------------------------------------------+").ok();
    writeln!(out, "|    CRYPTOCRAFT  Pool Server   {}:{:<5}                      |", st.bind_ip, POOL_PORT).ok();
    writeln!(out, "+------------------------------------------------------------------+").ok();
    execute!(out, ResetColor).ok();

    execute!(out, SetForegroundColor(Color::Green)).ok();
    writeln!(out, "  Chain Length  : {:<10}  Difficulty : {} zeros", st.blockchain.chain.len(), st.blockchain.difficulty).ok();
    writeln!(out, "  Blocks Found  : {:<10}  CC Mined   : {:.4} CC", st.blocks_total, st.blockchain.total_mined).ok();
    writeln!(out, "  Pool Hashrate : {}  Dashboard : http://{}:{}/", fmt_hr(st.total_hashrate()), st.bind_ip, DASHBOARD_PORT).ok();
    execute!(out, ResetColor).ok();

    writeln!(out, "--------------------------------------------------------------------").ok();
    execute!(out, SetForegroundColor(Color::Cyan)).ok();
    writeln!(out, "  Connected Miners ({}):", st.miners.len()).ok();
    execute!(out, ResetColor).ok();
    writeln!(out, "  {:<20} {:<16} {:<12} {:<10}", "Name", "Hashrate", "Blocks", "Uptime").ok();
    writeln!(out, "  {:-<20} {:-<16} {:-<12} {:-<10}", "", "", "", "").ok();

    let mut miners: Vec<_> = st.miners.values().collect();
    miners.sort_by(|a, b| b.hashrate.cmp(&a.hashrate));
    for m in &miners {
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "  {:<20} {:<16} {:<12} {:<10}", trunc(&m.name,19), fmt_hr(m.hashrate), m.blocks_found, fmt_up(m.joined.elapsed().as_secs())).ok();
    }
    for _ in 0..(5usize.saturating_sub(miners.len())) { writeln!(out, "{:70}", "").ok(); }
    execute!(out, ResetColor).ok();

    writeln!(out, "--------------------------------------------------------------------").ok();
    execute!(out, SetForegroundColor(Color::Green)).ok();
    writeln!(out, "  Recent Blocks:").ok();
    execute!(out, ResetColor).ok();
    let recent: Vec<_> = st.blockchain.chain.iter().rev().skip(1).take(4).collect();
    for b in &recent {
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "    #{:<5} | {}…{} | {} zeros | {:.4} CC | {}", b.index, &b.hash[..8], &b.hash[56..], b.hash.chars().take_while(|&c|c=='0').count(), b.reward, &b.miner).ok();
    }
    for _ in 0..(4usize.saturating_sub(recent.len())) { writeln!(out, "{:70}", "").ok(); }
    execute!(out, ResetColor).ok();

    writeln!(out, "--------------------------------------------------------------------").ok();
    execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
    writeln!(out, "  [Q + Enter] Back to menu                                          ").ok();
    execute!(out, ResetColor).ok();
    out.flush().ok();
}

// ── HTTP header helpers ───────────────────────────────────────────────────────

fn ct(s: &str) -> Header { Header::from_bytes("Content-Type", s).unwrap() }
fn cors()      -> Header { Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap() }
fn no_cache()  -> Header { Header::from_bytes("Cache-Control", "no-cache, no-store").unwrap() }

// ── Misc helpers ─────────────────────────────────────────────────────────────

fn fmt_hr(hr: u64) -> String {
    if hr >= 1_000_000 { format!("{:.2} MH/s", hr as f64/1_000_000.0) }
    else if hr >= 1_000 { format!("{:.2} KH/s", hr as f64/1_000.0) }
    else { format!("{} H/s", hr) }
}
fn fmt_up(s: u64) -> String {
    if s < 60 { format!("{}s", s) } else if s < 3600 { format!("{}m {}s", s/60, s%60) } else { format!("{}h {}m", s/3600, (s%3600)/60) }
}
fn trunc(s: &str, n: usize) -> &str { if s.len() <= n { s } else { &s[..n] } }
