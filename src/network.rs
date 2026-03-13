// ─── pool_server.rs ───────────────────────────────────────────────────────────
// TCP mining pool server. Binds to 0.0.0.0:8080 (all interfaces).
// Distributes block templates to connected miners, accepts the first valid
// submission, adds it to the blockchain, then broadcasts new work.
// ─────────────────────────────────────────────────────────────────────────────

use crate::blockchain::{Block, Blockchain};
use crossterm::{cursor, execute, style::{Color, ResetColor, SetForegroundColor}, terminal::{self, ClearType}};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const POOL_PORT: u16  = 8080;
pub const POOL_HOST: &str = "0.0.0.0";

// ── Wire protocol (newline-delimited JSON) ────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Work   { index: u64, prev_hash: String, difficulty: usize, timestamp: u64, data: String, reward: f64 },
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

// ── Shared pool state ─────────────────────────────────────────────────────────

pub struct PoolState {
    pub blockchain:   Blockchain,
    pub save_file:    String,
    pub miners:       HashMap<String, MinerConn>,
    pub blocks_total: u64,
    pub bind_ip:      String,
}

impl PoolState {
    fn broadcast(&self, msg: &ServerMsg) {
        for conn in self.miners.values() {
            conn.send(msg);
        }
    }

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
}

// ── Server entry point ────────────────────────────────────────────────────────

pub fn run(blockchain: Blockchain, save_file: String, bind_ip: String) {
    let state = Arc::new(Mutex::new(PoolState {
        blockchain,
        save_file,
        miners: HashMap::new(),
        blocks_total: 0,
        bind_ip: bind_ip.clone(),
    }));

    let addr     = format!("{}:{}", bind_ip, POOL_PORT);
    let listener = TcpListener::bind(&addr).expect("Failed to bind pool server");

    // Spawn UI refresh thread
    {
        let s = state.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            if let Ok(st) = s.lock() { draw_server_ui(&st); }
        });
    }

    // Accept loop
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let s2 = state.clone();
                std::thread::spawn(move || handle_client(s, s2));
            }
            Err(_) => {}
        }
    }
}

// ── Client handler (one thread per connection) ────────────────────────────────

fn handle_client(stream: TcpStream, state: Arc<Mutex<PoolState>>) {
    let addr   = stream.peer_addr().map(|a| a.to_string()).unwrap_or("?".into());
    let writer = Arc::new(Mutex::new(stream.try_clone().expect("clone stream")));
    let reader = BufReader::new(stream);

    let mut email = String::from("unknown");
    let mut name  = String::from("unknown");
    let mut authed = false;

    // Send current work immediately after Hello
    let send_work = |w: &Arc<Mutex<TcpStream>>, st: &Arc<Mutex<PoolState>>| {
        if let Ok(st) = st.lock() {
            let msg  = st.build_work();
            let line = serde_json::to_string(&msg).unwrap_or_default() + "\n";
            if let Ok(mut w) = w.lock() { let _ = w.write_all(line.as_bytes()); }
        }
    };

    for line in reader.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }

        let msg: ClientMsg = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match msg {
            // ── Handshake ────────────────────────────────────────────────────
            ClientMsg::Hello { email: e, name: n } => {
                email  = e;
                name   = n.clone();
                authed = true;

                let conn = MinerConn {
                    email:        email.clone(),
                    name:         name.clone(),
                    hashrate:     0,
                    blocks_found: 0,
                    joined:       Instant::now(),
                    writer:       writer.clone(),
                };

                if let Ok(mut st) = state.lock() {
                    st.miners.insert(addr.clone(), conn);
                }
                send_work(&writer, &state);
            }

            // ── Block submission ─────────────────────────────────────────────
            ClientMsg::Submit { index, nonce, hash } => {
                if !authed { continue; }

                let mut st = match state.lock() { Ok(s) => s, Err(_) => continue };

                // Validate: correct index, valid hash prefix
                let expected_index = st.blockchain.next_index();
                let difficulty     = st.blockchain.difficulty;
                let prefix         = "0".repeat(difficulty);

                if index != expected_index {
                    let msg = ServerMsg::Rejected { reason: "stale block".into() };
                    if let Ok(mut w) = writer.lock() {
                        let _ = w.write_all((serde_json::to_string(&msg).unwrap() + "\n").as_bytes());
                    }
                    continue;
                }

                if !hash.starts_with(&prefix) {
                    let msg = ServerMsg::Rejected { reason: "invalid hash".into() };
                    if let Ok(mut w) = writer.lock() {
                        let _ = w.write_all((serde_json::to_string(&msg).unwrap() + "\n").as_bytes());
                    }
                    continue;
                }

                // Accept — add block
                let block  = st.blockchain.add_block(nonce, hash, 0);
                st.blockchain.save(&st.save_file);
                st.blocks_total += 1;

                // Credit the winning miner
                if let Some(conn) = st.miners.get_mut(&addr) {
                    conn.blocks_found += 1;
                }

                // Tell winner
                let accept = ServerMsg::Accepted { block_index: block.index, reward: block.reward };
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all((serde_json::to_string(&accept).unwrap() + "\n").as_bytes());
                }

                // Stop all, then send new work
                let stop = ServerMsg::Stop;
                let work = st.build_work();
                for (a, conn) in &st.miners {
                    if a != &addr { conn.send(&stop); }
                    conn.send(&work);
                }
            }

            // ── Hashrate report ──────────────────────────────────────────────
            ClientMsg::Hashrate { hr } => {
                if let Ok(mut st) = state.lock() {
                    if let Some(conn) = st.miners.get_mut(&addr) {
                        conn.hashrate = hr;
                    }
                }
            }

            ClientMsg::Bye => break,
        }
    }

    // Disconnected
    if let Ok(mut st) = state.lock() {
        st.miners.remove(&addr);
    }
}

// ── Server terminal UI ────────────────────────────────────────────────────────

fn draw_server_ui(st: &PoolState) {
    let mut out = io::stdout();
    execute!(out, cursor::MoveTo(0, 0)).ok();

    execute!(out, SetForegroundColor(Color::Yellow)).ok();
    writeln!(out, "+------------------------------------------------------------------+").ok();
    writeln!(out, "|    CRYPTOCRAFT  Pool Server   {}:{:<5}                      |",
        st.bind_ip, POOL_PORT).ok();
    writeln!(out, "+------------------------------------------------------------------+").ok();
    execute!(out, ResetColor).ok();

    // Chain stats
    execute!(out, SetForegroundColor(Color::Green)).ok();
    writeln!(out, "  Chain Length  : {:<10}  Difficulty : {} zeros",
        st.blockchain.chain.len(), st.blockchain.difficulty).ok();
    writeln!(out, "  Blocks Found  : {:<10}  CC Mined   : {:.4} CC",
        st.blocks_total, st.blockchain.total_mined).ok();
    writeln!(out, "  Pool Hashrate : {}",
        fmt_hr(st.total_hashrate())).ok();
    execute!(out, ResetColor).ok();

    writeln!(out, "--------------------------------------------------------------------").ok();

    // Miner table
    execute!(out, SetForegroundColor(Color::Cyan)).ok();
    writeln!(out, "  Connected Miners ({}):", st.miners.len()).ok();
    execute!(out, ResetColor).ok();

    writeln!(out, "  {:<20} {:<16} {:<12} {:<10}", "Name", "Hashrate", "Blocks", "Uptime").ok();
    writeln!(out, "  {:-<20} {:-<16} {:-<12} {:-<10}", "", "", "", "").ok();

    let mut miners: Vec<_> = st.miners.values().collect();
    miners.sort_by(|a, b| b.hashrate.cmp(&a.hashrate));

    for m in &miners {
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "  {:<20} {:<16} {:<12} {:<10}",
            truncate(&m.name, 19),
            fmt_hr(m.hashrate),
            m.blocks_found,
            fmt_uptime(m.joined.elapsed().as_secs()),
        ).ok();
    }
    execute!(out, ResetColor).ok();

    // Pad empty rows so layout is stable
    for _ in 0..(5usize.saturating_sub(miners.len())) {
        writeln!(out, "{:70}", "").ok();
    }

    writeln!(out, "--------------------------------------------------------------------").ok();

    // Recent blocks
    execute!(out, SetForegroundColor(Color::Green)).ok();
    writeln!(out, "  Recent Blocks:").ok();
    execute!(out, ResetColor).ok();

    let recent: Vec<_> = st.blockchain.chain.iter().rev().skip(1).take(4).collect();
    for b in &recent {
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "    #{:<5} | {}...{} | {} zeros | {:.4} CC | {}",
            b.index,
            &b.hash[..8], &b.hash[56..],
            b.hash.chars().take_while(|&c| c == '0').count(),
            b.reward,
            &b.miner,
        ).ok();
    }
    for _ in 0..(4usize.saturating_sub(recent.len())) {
        writeln!(out, "{:70}", "").ok();
    }
    execute!(out, ResetColor).ok();

    writeln!(out, "--------------------------------------------------------------------").ok();
    execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
    writeln!(out, "  [Ctrl+C] Stop server   Miners connect to: {}:{}       ", st.bind_ip, POOL_PORT).ok();
    execute!(out, ResetColor).ok();

    out.flush().ok();
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fmt_hr(hr: u64) -> String {
    if hr >= 1_000_000 { format!("{:.2} MH/s", hr as f64 / 1_000_000.0) }
    else if hr >= 1_000 { format!("{:.2} KH/s", hr as f64 / 1_000.0) }
    else { format!("{} H/s", hr) }
}

fn fmt_uptime(s: u64) -> String {
    if s < 60 { format!("{}s", s) }
    else if s < 3600 { format!("{}m {}s", s/60, s%60) }
    else { format!("{}h {}m", s/3600, (s%3600)/60) }
}

fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n { s } else { &s[..n] }
}
