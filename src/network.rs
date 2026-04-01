// ─── network.rs ───────────────────────────────────────────────────────────────
// Network interface detection, selection, and pool auto-discovery.
// ─────────────────────────────────────────────────────────────────────────────

use crate::pool_server::{DISCOVERY_PING, DISCOVERY_PONG, DISCOVERY_PORT};
use crossterm::{execute, style::{Color, ResetColor, SetForegroundColor}};
use std::{
    io::{self, Write},
    net::UdpSocket,
    time::Duration,
};

#[derive(Debug, Clone)]
pub struct Iface {
    pub name: String,     // e.g. "Wi-Fi" or "Ethernet"
    pub ip:   String,     // e.g. "192.168.1.2"
    pub kind: IfaceKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IfaceKind {
    Ethernet,
    Wifi,
    Other,
}

impl std::fmt::Display for IfaceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IfaceKind::Ethernet => write!(f, "Ethernet"),
            IfaceKind::Wifi     => write!(f, "Wi-Fi   "),
            IfaceKind::Other    => write!(f, "Other   "),
        }
    }
}

/// Enumerate all non-loopback IPv4 interfaces.
pub fn list_interfaces() -> Vec<Iface> {
    let mut ifaces = Vec::new();

    if let Ok(all) = if_addrs::get_if_addrs() {
        for iface in all {
            // Skip loopback and IPv6
            if iface.is_loopback() { continue; }
            let ip = match iface.ip() {
                std::net::IpAddr::V4(v4) => v4.to_string(),
                _ => continue,
            };

            let name_lower = iface.name.to_lowercase();
            let kind = if name_lower.contains("wi-fi")
                || name_lower.contains("wifi")
                || name_lower.contains("wlan")
                || name_lower.contains("wireless")
            {
                IfaceKind::Wifi
            } else if name_lower.contains("ethernet")
                || name_lower.contains("eth")
                || name_lower.contains("local area")
            {
                IfaceKind::Ethernet
            } else {
                IfaceKind::Other
            };

            ifaces.push(Iface { name: iface.name, ip, kind });
        }
    }

    // Sort: Ethernet first, then WiFi, then Other
    ifaces.sort_by_key(|i| match i.kind {
        IfaceKind::Ethernet => 0,
        IfaceKind::Wifi     => 1,
        IfaceKind::Other    => 2,
    });

    ifaces
}

/// Interactive prompt — user picks which interface to HOST on.
/// Returns (bind_ip, display_ip).
pub fn pick_host_interface() -> (String, String) {
    let ifaces = list_interfaces();

    if ifaces.is_empty() {
        // Nothing detected — fall back to manual entry
        println!("  No network interfaces detected automatically.");
        return prompt_manual_ip("  Enter this machine's IP address to bind on");
    }

    println!();
    execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
    println!("  +--------------------------------------------------+");
    println!("  |   Select Network Interface to Host On            |");
    println!("  +--------------------------------------------------+");
    execute!(io::stdout(), ResetColor).ok();
    println!();

    for (i, iface) in ifaces.iter().enumerate() {
        execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
        print!("  [{}] ", i + 1);
        execute!(io::stdout(), SetForegroundColor(Color::White)).ok();
        print!("{} ", iface.kind);
        execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
        print!("{:<18}", iface.ip);
        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("  ({})", iface.name);
    }

    execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
    println!("  [M] Enter IP manually");
    execute!(io::stdout(), ResetColor).ok();
    println!();

    loop {
        print!("  Choice: ");
        io::stdout().flush().ok();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).ok();
        let choice = buf.trim().to_lowercase();

        if choice == "m" {
            return prompt_manual_ip("  Enter IP to bind on");
        }

        if let Ok(n) = choice.parse::<usize>() {
            if n >= 1 && n <= ifaces.len() {
                let ip = ifaces[n - 1].ip.clone();
                return (ip.clone(), ip);
            }
        }

        println!("  Invalid choice, try again.");
    }
}

/// Try to find a CryptoCraft pool server on the local network via UDP broadcast.
/// Sends CRYPTOCRAFT_DISCOVER_V1, waits up to `timeout_secs` for a reply.
/// Returns the server address string e.g. "192.168.1.2:8080" if found.
pub fn discover_pool(timeout_secs: u64) -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_broadcast(true).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(timeout_secs))).ok()?;

    let target = format!("255.255.255.255:{}", DISCOVERY_PORT);
    sock.send_to(DISCOVERY_PING.as_bytes(), &target).ok()?;

    let mut buf = [0u8; 128];
    if let Ok((len, _src)) = sock.recv_from(&mut buf) {
        let reply = std::str::from_utf8(&buf[..len]).unwrap_or("");
        // Expected format: "CRYPTOCRAFT_POOL_V1|192.168.1.2:8080"
        if let Some(addr) = reply.strip_prefix(&format!("{}|", DISCOVERY_PONG)) {
            return Some(addr.trim().to_string());
        }
    }
    None
}

/// Interactive prompt — auto-discovers pool or lets user enter IP manually.
pub fn pick_server_address(default_port: u16) -> String {
    let ifaces = list_interfaces();

    println!();
    execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
    println!("  +--------------------------------------------------+");
    println!("  |   Connect to Mining Pool Server                  |");
    println!("  +--------------------------------------------------+");
    execute!(io::stdout(), ResetColor).ok();
    println!();

    // Show this machine's interfaces for reference
    if !ifaces.is_empty() {
        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("  This machine's interfaces:");
        for iface in &ifaces {
            println!("    {} {}  ({})", iface.kind, iface.ip, iface.name);
        }
        println!();
        execute!(io::stdout(), ResetColor).ok();
    }

    // Try auto-discovery first
    execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
    println!("  Searching for pool server on local network...");
    execute!(io::stdout(), ResetColor).ok();
    io::stdout().flush().ok();

    // Try UDP broadcast first (fast)
    let found = if let Some(addr) = discover_pool(2) {
        Some(addr)
    } else {
        // UDP failed — try subnet scan (slower but more reliable)
        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("  UDP search failed, scanning subnet...");
        execute!(io::stdout(), ResetColor).ok();
        io::stdout().flush().ok();
        scan_for_pool(default_port)
    };

    if let Some(addr) = found {
        execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
        println!("  Found pool server at: {}", addr);
        execute!(io::stdout(), ResetColor).ok();
        println!();
        print!("  Connect to {}? [Y/n]: ", addr);
        io::stdout().flush().ok();

        let mut buf = String::new();
        io::stdin().read_line(&mut buf).ok();
        let answer = buf.trim().to_lowercase();

        if answer.is_empty() || answer == "y" || answer == "yes" {
            // Save for sync.rs to reuse
            let _ = std::fs::write(crate::sync::SERVER_CONFIG_FILE,
                format!("{}:{}", addr.split(':').next().unwrap_or(""), 2700));
            return addr;
        }
    } else {
        execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
        println!("  No pool found automatically.");
        execute!(io::stdout(), ResetColor).ok();
    }

    // Fall back to manual entry
    println!();
    execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
    print!("  Enter server IP (default 192.168.1.2): ");
    execute!(io::stdout(), ResetColor).ok();
    io::stdout().flush().ok();

    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
    let ip = buf.trim().to_string();
    let ip = if ip.is_empty() { "192.168.1.2".to_string() } else { ip };

    format!("{}:{}", ip, default_port)
}

/// Scan local subnets for a CryptoCraft pool server by trying port directly.
/// Much more reliable than UDP broadcast on consumer routers.
pub fn scan_for_pool(pool_port: u16) -> Option<String> {
    use std::net::TcpStream;
    use std::time::Duration;

    let subnets = ["192.168.1", "192.168.0", "10.0.0", "10.0.1"];

    for subnet in &subnets {
        for host in 1u8..=254 {
            let addr = format!("{}.{}:{}", subnet, host, pool_port);
            if let Ok(parsed) = addr.parse() {
                if TcpStream::connect_timeout(&parsed, Duration::from_millis(80)).is_ok() {
                    return Some(addr);
                }
            }
        }
    }
    None
}
    print!("  {}: ", label);
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
    let ip = buf.trim().to_string();
    let ip = if ip.is_empty() { "0.0.0.0".to_string() } else { ip };
    (ip.clone(), ip)
}
