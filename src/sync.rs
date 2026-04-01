// ─── sync.rs ──────────────────────────────────────────────────────────────────
// Handles syncing a solo miner's offline rewards to the pool server's SQLite.
//
// On solo miner startup:
//   1. Try to reach the server (auto-discover via UDP or use last known IP)
//   2. If reachable, POST /sync with any blocks mined since last sync
//   3. Server credits those rewards into its own SQLite database
//   4. Update blockchain.synced_through and save
//   5. If unreachable, proceed silently — will retry next launch
//
// This is fully automatic and silent to the user.
// ─────────────────────────────────────────────────────────────────────────────

use crate::blockchain::Blockchain;
use crate::network::discover_pool;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
    fs,
    path::Path,
};

pub const SERVER_CONFIG_FILE: &str = "cryptocraft_server.txt";
const SYNC_TIMEOUT_SECS:  u64  = 4;

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SyncRequest {
    pub email:  String,
    pub blocks: Vec<SyncBlock>,
}

#[derive(Serialize)]
pub struct SyncBlock {
    pub index:     u64,
    pub reward:    f64,
    pub timestamp: u64,
    pub hash:      String,
}

#[derive(Deserialize)]
pub struct SyncResponse {
    pub ok:              bool,
    pub blocks_credited: u64,
    pub message:         String,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Try to find and sync to the server. Fully silent — returns true if synced.
/// Call this on solo miner startup before the mining loop begins.
pub fn try_sync(blockchain: &mut Blockchain, email: &str, chain_file: &str) -> bool {
    let server_ip = match find_server_ip() {
        Some(ip) => ip,
        None     => return false,
    };

    // Save discovered IP for next time
    save_server_ip(&server_ip);

    // Collect unsynced blocks
    let unsynced: Vec<SyncBlock> = blockchain.chain.iter()
        .filter(|b| b.index > 0 && b.index > blockchain.synced_through && b.reward > 0.0)
        .map(|b| SyncBlock {
            index:     b.index,
            reward:    b.reward,
            timestamp: b.timestamp,
            hash:      b.hash.clone(),
        })
        .collect();

    if unsynced.is_empty() {
        // Nothing to sync but server is reachable — still a success
        return true;
    }

    let count = unsynced.len();
    let last  = unsynced.last().map(|b| b.index).unwrap_or(0);

    let payload = SyncRequest {
        email:  email.to_string(),
        blocks: unsynced,
    };

    match post_sync(&server_ip, &payload) {
        Some(resp) if resp.ok => {
            blockchain.synced_through = last;
            blockchain.save(chain_file);
            eprintln!("[sync] Synced {} blocks to server.", count);
            true
        }
        _ => false,
    }
}

/// Quick check — is the server reachable right now?
/// Used by wallet to gate transfers.
pub fn server_reachable() -> bool {
    find_server_ip().is_some()
}

/// Fetch the authoritative balance for an email from the server.
/// Returns None if server unreachable or user not found there.
pub fn fetch_server_balance(email: &str) -> Option<f64> {
    let server = find_server_ip()?;
    let url    = format!("http://{}/balance?email={}", server,
        email.replace('@', "%40").replace('.', "%2E"));
    let body   = http_get(&url, SYNC_TIMEOUT_SECS)?;
    // Response: { "balance": 12.5 }
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v["balance"].as_f64()
}

/// Get the last known server IP from disk, or None.
pub fn last_known_server() -> Option<String> {
    if Path::new(SERVER_CONFIG_FILE).exists() {
        fs::read_to_string(SERVER_CONFIG_FILE).ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Try to find the server using multiple strategies in order:
/// 1. Last known saved IP (instant, most reliable)
/// 2. UDP broadcast discovery
/// 3. Direct subnet scan of 192.168.x.1-254 on port 2700
fn find_server_ip() -> Option<String> {
    // Strategy 1: last known IP (saved from previous successful connection)
    if let Some(ip) = last_known_server() {
        if ping_server(&ip) {
            return Some(ip);
        }
    }

    // Strategy 2: UDP broadcast (fast but unreliable on some routers)
    if let Some(addr) = discover_pool(2) {
        let ip: String = addr.split(':').next()?.to_string();
        let dashboard_addr = format!("{}:2700", ip);
        if ping_server(&dashboard_addr) {
            save_server_ip(&dashboard_addr);
            return Some(dashboard_addr);
        }
    }

    // Strategy 3: Direct subnet scan — try common gateway subnets
    // Uses very short timeout per host so scan finishes in a few seconds
    let subnets = ["192.168.1", "192.168.0", "10.0.0", "10.0.1"];
    for subnet in &subnets {
        for host in 1u8..=254 {
            let addr = format!("{}.{}:2700", subnet, host);
            if let Ok(parsed) = addr.parse() {
                if std::net::TcpStream::connect_timeout(
                    &parsed,
                    Duration::from_millis(80)
                ).is_ok() {
                    if ping_server(&addr) {
                        save_server_ip(&addr);
                        return Some(addr);
                    }
                }
            }
        }
    }

    None
}

/// Check if the server's HTTP dashboard is alive by hitting /status.
fn ping_server(addr: &str) -> bool {
    let url = format!("http://{}/status", addr);
    http_get(&url, SYNC_TIMEOUT_SECS).is_some()
}

fn save_server_ip(addr: &str) {
    let _ = fs::write(SERVER_CONFIG_FILE, addr);
}

/// POST the sync request to /sync on the server's dashboard port.
fn post_sync(server_addr: &str, payload: &SyncRequest) -> Option<SyncResponse> {
    let body = serde_json::to_string(payload).ok()?;
    let host = server_addr;

    let mut stream = TcpStream::connect(host).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(SYNC_TIMEOUT_SECS))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(SYNC_TIMEOUT_SECS))).ok();

    let request = format!(
        "POST /sync HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        host, body.len(), body
    );

    stream.write_all(request.as_bytes()).ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;

    let body_start = response.find("\r\n\r\n")?;
    let json = &response[body_start + 4..];
    serde_json::from_str(json).ok()
}

/// Simple blocking HTTP GET, returns body or None.
fn http_get(url: &str, timeout_secs: u64) -> Option<String> {
    http_get_timeout(url, timeout_secs)
}

/// HTTP GET with a specific timeout in seconds.
fn http_get_timeout(url: &str, timeout_secs: u64) -> Option<String> {
    let without_scheme = url.strip_prefix("http://")?;
    let (host, path) = if let Some(pos) = without_scheme.find('/') {
        (&without_scheme[..pos], &without_scheme[pos..])
    } else {
        (without_scheme, "/")
    };

    let mut stream = TcpStream::connect(host).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(timeout_secs))).ok();

    let request = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, host
    );
    stream.write_all(request.as_bytes()).ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;

    response.find("\r\n\r\n")
        .map(|pos| response[pos + 4..].to_string())
}
