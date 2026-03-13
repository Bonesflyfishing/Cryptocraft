// ─── network.rs ───────────────────────────────────────────────────────────────
// Network interface detection and selection.
// Lists WiFi and Ethernet interfaces separately so the user can pick which
// one to host/join on.
// ─────────────────────────────────────────────────────────────────────────────

use crossterm::{execute, style::{Color, ResetColor, SetForegroundColor}};
use std::io::{self, Write};

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

/// Interactive prompt — user enters or picks the server IP to connect to.
pub fn pick_server_address(default_port: u16) -> String {
    let ifaces = list_interfaces();

    println!();
    execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
    println!("  +--------------------------------------------------+");
    println!("  |   Connect to Mining Pool Server                  |");
    println!("  +--------------------------------------------------+");
    execute!(io::stdout(), ResetColor).ok();
    println!();

    if !ifaces.is_empty() {
        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("  This machine's interfaces (for reference):");
        for iface in &ifaces {
            println!("    {} {}  ({})", iface.kind, iface.ip, iface.name);
        }
        println!();
        execute!(io::stdout(), ResetColor).ok();
    }

    execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
    print!("  Server IP (default 192.168.1.2): ");
    execute!(io::stdout(), ResetColor).ok();
    io::stdout().flush().ok();

    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
    let ip = buf.trim().to_string();
    let ip = if ip.is_empty() { "192.168.1.2".to_string() } else { ip };

    format!("{}:{}", ip, default_port)
}

fn prompt_manual_ip(label: &str) -> (String, String) {
    print!("  {}: ", label);
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
    let ip = buf.trim().to_string();
    let ip = if ip.is_empty() { "0.0.0.0".to_string() } else { ip };
    (ip.clone(), ip)
}
