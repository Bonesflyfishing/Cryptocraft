// ─── server.rs ────────────────────────────────────────────────────────────────
// Tiny background HTTP server.
// Runs on port 2700 (Ryzen 2700 — seemed fitting).
//
// Routes:
//   GET /          → dashboard.html
//   GET /chain     → current chain JSON (CORS open for local network)
//   GET /status    → simple alive ping { "mining": true }
// ──────────────────────────────────────────────────────────────────────────────

use std::{
    fs,
    io::Cursor,
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};
use tiny_http::{Header, Response, Server};

pub const PORT: u16 = 2700;

/// Shared state the server reads from. Updated by the miner after each block.
#[derive(Clone)]
pub struct ServerState {
    pub chain_file: Arc<Mutex<String>>,  // path to the active chain JSON file
}

impl ServerState {
    pub fn new(chain_file: &str) -> Self {
        ServerState {
            chain_file: Arc::new(Mutex::new(chain_file.to_string())),
        }
    }

    pub fn update_chain_file(&self, path: &str) {
        if let Ok(mut f) = self.chain_file.lock() {
            *f = path.to_string();
        }
    }
}

/// Spawn the server on a background thread. Returns immediately.
pub fn spawn(state: ServerState, dashboard_html: String) {
    thread::spawn(move || {
        let addr = format!("0.0.0.0:{}", PORT);
        let server = match Server::http(&addr) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[server] Failed to bind on {}: {}", addr, e);
                return;
            }
        };

        let dashboard_bytes = Arc::new(dashboard_html.into_bytes());

        for request in server.incoming_requests() {
            let url  = request.url().to_string();
            let path = url.split('?').next().unwrap_or("/");

            match path {
                // ── Dashboard HTML ──────────────────────────────────────────
                "/" | "/index.html" => {
                    let body = dashboard_bytes.clone();
                    let resp = Response::from_data(body.as_ref().to_vec())
                        .with_header(content_type("text/html; charset=utf-8"))
                        .with_header(no_cache());
                    let _ = request.respond(resp);
                }

                // ── Chain JSON ──────────────────────────────────────────────
                "/chain" => {
                    let file_path = state.chain_file.lock()
                        .map(|f| f.clone())
                        .unwrap_or_default();

                    let body = fs::read_to_string(&file_path)
                        .unwrap_or_else(|_| r#"{"error":"chain file not found"}"#.to_string());

                    let resp = Response::from_data(body.into_bytes())
                        .with_header(content_type("application/json"))
                        .with_header(cors())
                        .with_header(no_cache());
                    let _ = request.respond(resp);
                }

                // ── Status ping ─────────────────────────────────────────────
                "/status" => {
                    let body = r#"{"mining":true,"version":"1.0.0"}"#;
                    let resp = Response::from_data(body.as_bytes().to_vec())
                        .with_header(content_type("application/json"))
                        .with_header(cors());
                    let _ = request.respond(resp);
                }

                // ── 404 ─────────────────────────────────────────────────────
                _ => {
                    let body = b"404 not found".to_vec();
                    let resp = Response::from_data(body).with_status_code(404);
                    let _ = request.respond(resp);
                }
            }
        }
    });
}

// ─── Header helpers ───────────────────────────────────────────────────────────

fn content_type(ct: &str) -> Header {
    Header::from_bytes("Content-Type", ct).unwrap()
}

fn cors() -> Header {
    Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap()
}

fn no_cache() -> Header {
    Header::from_bytes("Cache-Control", "no-cache, no-store").unwrap()
}

/// Try to get the machine's LAN IP for display purposes.
pub fn local_ip() -> String {
    // Connect to a public address (doesn't actually send data) to find our LAN IP
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "localhost".to_string()
}