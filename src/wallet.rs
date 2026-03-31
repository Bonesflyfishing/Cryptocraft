// ─── wallet.rs ────────────────────────────────────────────────────────────────
// Interactive wallet menu. Shows balance, transaction history, and lets the
// user send CC to other accounts.
// ─────────────────────────────────────────────────────────────────────────────

use crate::db::{self, Db};
use crate::auth::Session;
use crate::sync;
use crossterm::{execute, style::{Color, ResetColor, SetForegroundColor}};
use std::io::{self, Write};
use std::time::{UNIX_EPOCH, Duration, SystemTime};

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run(db: &Db, session: &Session) {
    loop {
        let online   = sync::server_reachable();
        let balance  = if online {
            // Use server's authoritative balance when connected
            sync::fetch_server_balance(&session.email)
                .unwrap_or_else(|| db::get_balance(db, &session.user_id))
        } else {
            db::get_balance(db, &session.user_id)
        };

        clear();
        execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
        println!("  ╔══════════════════════════════════════════╗");
        println!("  ║   CryptoCraft Wallet                     ║");
        println!("  ╚══════════════════════════════════════════╝");
        execute!(io::stdout(), ResetColor).ok();
        println!();
        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("  Account  : {}", session.email);
        execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
        println!("  Balance  : {:.4} CC", balance);
        if online {
            execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
            println!("  Server   : ONLINE");
        } else {
            execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
            println!("  Server   : OFFLINE  (transfers unavailable)");
        }
        execute!(io::stdout(), ResetColor).ok();
        println!();
        execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
        println!("  [1] Send CC{}", if !online { " (requires server)" } else { "" });
        println!("  [2] Transaction history");
        println!("  [3] Check another user's balance");
        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("  [4] Back to menu");
        execute!(io::stdout(), ResetColor).ok();
        println!();

        print!("  Choice: ");
        io::stdout().flush().ok();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).ok();

        match buf.trim() {
            "1" => send_cc(db, session),
            "2" => show_history(db, session),
            "3" => check_balance(db),
            "4" | _ => break,
        }
    }
}

// ── Send CC ───────────────────────────────────────────────────────────────────

fn send_cc(db: &Db, session: &Session) {
    clear();
    execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
    println!("  Send CryptoCraft Coins");
    println!("  ─────────────────────");
    execute!(io::stdout(), ResetColor).ok();
    println!();

    // Require server connection for transfers — balances live on the server
    execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
    print!("  Checking server connection...");
    io::stdout().flush().ok();
    execute!(io::stdout(), ResetColor).ok();

    if !sync::server_reachable() {
        println!();
        execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
        println!("  Cannot send CC — server is not reachable.");
        println!("  Transfers require a connection to the pool server.");
        println!("  Start the pool server on your network and try again.");
        execute!(io::stdout(), ResetColor).ok();
        pause();
        return;
    }

    execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
    println!(" connected!");
    execute!(io::stdout(), ResetColor).ok();
    println!();

    let balance = db::get_balance(db, &session.user_id);
    execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
    println!("  Your balance: {:.4} CC", balance);
    execute!(io::stdout(), ResetColor).ok();
    println!();

    // Get recipient email
    print!("  Recipient email: ");
    io::stdout().flush().ok();
    let mut email_buf = String::new();
    io::stdin().read_line(&mut email_buf).ok();
    let recipient_email = email_buf.trim().to_lowercase();

    if recipient_email.is_empty() {
        println!("\n  Cancelled.");
        pause();
        return;
    }

    if recipient_email == session.email {
        execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
        println!("\n  You can't send CC to yourself.");
        execute!(io::stdout(), ResetColor).ok();
        pause();
        return;
    }

    // Look up recipient
    let receiver_id = match db::user_id_for_email(db, &recipient_email) {
        Some(id) => id,
        None => {
            execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
            println!("\n  No account found with that email.");
            execute!(io::stdout(), ResetColor).ok();
            pause();
            return;
        }
    };

    // Get amount
    print!("  Amount to send (CC): ");
    io::stdout().flush().ok();
    let mut amount_buf = String::new();
    io::stdin().read_line(&mut amount_buf).ok();
    let amount: f64 = match amount_buf.trim().parse() {
        Ok(a) => a,
        Err(_) => {
            execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
            println!("\n  Invalid amount.");
            execute!(io::stdout(), ResetColor).ok();
            pause();
            return;
        }
    };

    // Confirm
    println!();
    execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
    println!("  ┌─────────────────────────────────────┐");
    println!("  │  Confirm Transfer                   │");
    println!("  ├─────────────────────────────────────┤");
    println!("  │  To     : {:<27}│", recipient_email);
    println!("  │  Amount : {:<.4} CC{:<20}│", amount, "");
    println!("  │  After  : {:<.4} CC remaining{:<12}│", balance - amount, "");
    println!("  └─────────────────────────────────────┘");
    execute!(io::stdout(), ResetColor).ok();
    println!();
    print!("  Confirm? [y/N]: ");
    io::stdout().flush().ok();

    let mut confirm = String::new();
    io::stdin().read_line(&mut confirm).ok();
    if !confirm.trim().eq_ignore_ascii_case("y") {
        println!("\n  Cancelled.");
        pause();
        return;
    }

    let memo = format!("Transfer to {}", recipient_email);
    match db::transfer(db, &session.user_id, &receiver_id, amount, &memo) {
        Ok(_) => {
            execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
            println!("\n  ✓ Sent {:.4} CC to {}!", amount, recipient_email);
            let new_balance = db::get_balance(db, &session.user_id);
            println!("  New balance: {:.4} CC", new_balance);
            execute!(io::stdout(), ResetColor).ok();
        }
        Err(e) => {
            execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
            println!("\n  Error: {}", e);
            execute!(io::stdout(), ResetColor).ok();
        }
    }

    pause();
}

// ── Transaction history ───────────────────────────────────────────────────────

fn show_history(db: &Db, session: &Session) {
    clear();
    execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
    println!("  Transaction History — Last 20");
    println!("  ────────────────────────────");
    execute!(io::stdout(), ResetColor).ok();
    println!();

    let txs = db::get_transactions(db, &session.user_id, 20);

    if txs.is_empty() {
        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("  No transactions yet.");
        execute!(io::stdout(), ResetColor).ok();
    } else {
        for tx in &txs {
            let is_incoming = tx.sender_id.is_none() || tx.receiver_id == session.user_id;
            let sign        = if is_incoming { "+" } else { "-" };
            let ts          = fmt_timestamp(tx.timestamp);

            if is_incoming {
                execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
            } else {
                execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
            }

            println!("  {} {:.4} CC  │  {}  │  {}",
                sign, tx.amount, ts, tx.memo);
        }
        execute!(io::stdout(), ResetColor).ok();
    }

    println!();
    pause();
}

// ── Check another user's balance ──────────────────────────────────────────────

fn check_balance(db: &Db) {
    clear();
    print!("  Enter email to look up: ");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
    let email = buf.trim().to_lowercase();

    match db::user_id_for_email(db, &email) {
        Some(uid) => {
            let balance = db::get_balance(db, &uid);
            execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
            println!("\n  {}: {:.4} CC", email, balance);
            execute!(io::stdout(), ResetColor).ok();
        }
        None => {
            execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
            println!("\n  No account found with that email.");
            execute!(io::stdout(), ResetColor).ok();
        }
    }

    pause();
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn clear() {
    use crossterm::{cursor, execute, terminal::{self, ClearType}};
    execute!(io::stdout(), terminal::Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
}

fn pause() {
    println!();
    print!("  Press Enter to continue...");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
}

fn fmt_timestamp(ts: i64) -> String {
    // Simple formatting without chrono dependency
    let secs = ts as u64;
    let dt   = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    match dt.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let s     = d.as_secs();
            let days  = s / 86400;
            let hours = (s % 86400) / 3600;
            let mins  = (s % 3600) / 60;
            // Rough date: days since epoch to year/month/day
            let year  = 1970 + days / 365;
            let month = (days % 365) / 30 + 1;
            let day   = (days % 365) % 30 + 1;
            format!("{:04}-{:02}-{:02} {:02}:{:02}", year, month, day, hours, mins)
        }
        Err(_) => "—".to_string()
    }
}
