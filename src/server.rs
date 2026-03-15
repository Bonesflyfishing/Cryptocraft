// ─── server.rs ────────────────────────────────────────────────────────────────
// Tiny background HTTP server for solo mining mode.
// Runs on port 2700.
//
// Routes:
//   GET /        → dashboard.html
//   GET /chain   → current chain JSON
//   GET /status  → { "mode": "solo", "mining": true }
// ──────────────────────────────────────────────────────────────────────────────

use std::{
    fs,
    sync::{Arc, Mutex},
    thread,
};
use tiny_http::{Header, Response, Server};

pub const PORT: u16 = 2700;

#[derive(Clone)]
pub struct ServerState {
    pub chain_file: Arc<Mutex<String>>,
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
                "/" | "/index.html" => {
                    let body = dashboard_bytes.as_ref().to_vec();
                    let _ = request.respond(Response::from_data(body)
                        .with_header(content_type("text/html; charset=utf-8"))
                        .with_header(no_cache()));
                }

                "/chain" => {
                    let file_path = state.chain_file.lock()
                        .map(|f| f.clone()).unwrap_or_default();
                    let body = fs::read_to_string(&file_path)
                        .unwrap_or_else(|_| r#"{"error":"chain file not found"}"#.to_string());
                    let _ = request.respond(Response::from_data(body.into_bytes())
                        .with_header(content_type("application/json"))
                        .with_header(cors())
                        .with_header(no_cache()));
                }

                "/status" => {
                    let body = r#"{"mode":"solo","mining":true}"#;
                    let _ = request.respond(Response::from_data(body.as_bytes().to_vec())
                        .with_header(content_type("application/json"))
                        .with_header(cors()));
                }

                _ => {
                    let _ = request.respond(
                        Response::from_data(b"404 not found".to_vec()).with_status_code(404)
                    );
                }
            }
        }
    });
}

fn content_type(ct: &str) -> Header {
    Header::from_bytes("Content-Type", ct).unwrap()
}

fn cors() -> Header {
    Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap()
}

fn no_cache() -> Header {
    Header::from_bytes("Cache-Control", "no-cache, no-store").unwrap()
}

pub fn local_ip() -> String {
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "localhost".to_string()
}
