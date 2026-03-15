// ─── db.rs ────────────────────────────────────────────────────────────────────
// Single shared SQLite database for the entire application.
// Handles: users, balances, transactions.
//
// Tables:
//   users        — email + Argon2id password hash
//   balances     — one row per user, current CC balance
//   transactions — full history of mining rewards and transfers
// ─────────────────────────────────────────────────────────────────────────────

use rusqlite::{Connection, params};
use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const DB_FILE: &str = "cryptocraft_users.db";

// ── Shared handle ─────────────────────────────────────────────────────────────
// Wrap the connection in Arc<Mutex> so pool_server, auth, and wallet can all
// share it safely across threads.

pub type Db = Arc<Mutex<Connection>>;

pub fn open() -> Db {
    let conn = Connection::open(DB_FILE)
        .expect("Failed to open SQLite database");

    // Enable WAL mode — allows concurrent reads while a write is happening.
    // Important for pool_server (writing rewards) and dashboard (reading stats)
    // running at the same time.
    conn.execute_batch("PRAGMA journal_mode=WAL;")
        .expect("Failed to enable WAL mode");

    // Create all tables if they don't exist yet.
    // Safe to run on every launch — IF NOT EXISTS means no-op if already there.
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS users (
            user_id       TEXT PRIMARY KEY,
            email         TEXT UNIQUE NOT NULL,
            password_hash TEXT NOT NULL,
            created_at    INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS balances (
            user_id TEXT PRIMARY KEY REFERENCES users(user_id),
            balance REAL NOT NULL DEFAULT 0.0
        );

        CREATE TABLE IF NOT EXISTS transactions (
            id          TEXT PRIMARY KEY,
            sender_id   TEXT,        -- NULL for mining rewards
            receiver_id TEXT NOT NULL,
            amount      REAL NOT NULL,
            memo        TEXT NOT NULL,
            timestamp   INTEGER NOT NULL
        );
    ").expect("Failed to create tables");

    Arc::new(Mutex::new(conn))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ── Balance functions ─────────────────────────────────────────────────────────

/// Get a user's current CC balance. Returns 0.0 if no balance row yet.
pub fn get_balance(db: &Db, user_id: &str) -> f64 {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT balance FROM balances WHERE user_id = ?1",
        params![user_id],
        |row| row.get(0),
    ).unwrap_or(0.0)
}

/// Ensure a balance row exists for this user (creates it at 0.0 if missing).
pub fn ensure_balance_row(db: &Db, user_id: &str) {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO balances (user_id, balance) VALUES (?1, 0.0)",
        params![user_id],
    ).ok();
}

/// Credit a user's balance (add CC). Used for mining rewards.
/// Also records a transaction row.
pub fn credit(db: &Db, receiver_id: &str, amount: f64, memo: &str) -> Result<(), String> {
    let conn = db.lock().unwrap();

    // Use a SQLite transaction so credit + record are atomic —
    // either both happen or neither does.
    conn.execute(
        "INSERT OR IGNORE INTO balances (user_id, balance) VALUES (?1, 0.0)",
        params![receiver_id],
    ).map_err(|e| e.to_string())?;

    conn.execute(
        "UPDATE balances SET balance = balance + ?1 WHERE user_id = ?2",
        params![amount, receiver_id],
    ).map_err(|e| e.to_string())?;

    conn.execute(
        "INSERT INTO transactions (id, sender_id, receiver_id, amount, memo, timestamp)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
        params![Uuid::new_v4().to_string(), receiver_id, amount, memo, now_ts()],
    ).map_err(|e| e.to_string())?;

    Ok(())
}

/// Transfer CC from one user to another.
/// Returns Err if sender has insufficient balance or either user doesn't exist.
pub fn transfer(
    db:          &Db,
    sender_id:   &str,
    receiver_id: &str,
    amount:      f64,
    memo:        &str,
) -> Result<(), String> {
    if amount <= 0.0 {
        return Err("Amount must be greater than zero.".into());
    }

    let conn = db.lock().unwrap();

    // Check sender balance
    let sender_balance: f64 = conn.query_row(
        "SELECT balance FROM balances WHERE user_id = ?1",
        params![sender_id],
        |row| row.get(0),
    ).unwrap_or(0.0);

    if sender_balance < amount {
        return Err(format!(
            "Insufficient balance. You have {:.4} CC, tried to send {:.4} CC.",
            sender_balance, amount
        ));
    }

    // Check receiver exists
    let receiver_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM users WHERE user_id = ?1",
        params![receiver_id],
        |row| row.get::<_, i64>(0),
    ).map(|c| c > 0).unwrap_or(false);

    if !receiver_exists {
        return Err("Receiver account not found.".into());
    }

    // Deduct from sender
    conn.execute(
        "UPDATE balances SET balance = balance - ?1 WHERE user_id = ?2",
        params![amount, sender_id],
    ).map_err(|e| e.to_string())?;

    // Credit receiver (ensure their balance row exists first)
    conn.execute(
        "INSERT OR IGNORE INTO balances (user_id, balance) VALUES (?1, 0.0)",
        params![receiver_id],
    ).map_err(|e| e.to_string())?;

    conn.execute(
        "UPDATE balances SET balance = balance + ?1 WHERE user_id = ?2",
        params![amount, receiver_id],
    ).map_err(|e| e.to_string())?;

    // Record transaction
    conn.execute(
        "INSERT INTO transactions (id, sender_id, receiver_id, amount, memo, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            Uuid::new_v4().to_string(),
            sender_id,
            receiver_id,
            amount,
            memo,
            now_ts()
        ],
    ).map_err(|e| e.to_string())?;

    Ok(())
}

/// Get a user_id from an email address.
pub fn user_id_for_email(db: &Db, email: &str) -> Option<String> {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT user_id FROM users WHERE email = ?1",
        params![email.trim().to_lowercase()],
        |row| row.get(0),
    ).ok()
}

/// Get recent transactions for a user (both sent and received), newest first.
pub struct TxRecord {
    pub id:          String,
    pub sender_id:   Option<String>,
    pub receiver_id: String,
    pub amount:      f64,
    pub memo:        String,
    pub timestamp:   i64,
}

pub fn get_transactions(db: &Db, user_id: &str, limit: usize) -> Vec<TxRecord> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, sender_id, receiver_id, amount, memo, timestamp
         FROM transactions
         WHERE sender_id = ?1 OR receiver_id = ?1
         ORDER BY timestamp DESC
         LIMIT ?2"
    ).unwrap();

    stmt.query_map(params![user_id, limit as i64], |row| {
        Ok(TxRecord {
            id:          row.get(0)?,
            sender_id:   row.get(1)?,
            receiver_id: row.get(2)?,
            amount:      row.get(3)?,
            memo:        row.get(4)?,
            timestamp:   row.get(5)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Distribute a block reward proportionally across all miners by hashrate.
/// miners: Vec of (user_id, hashrate)
/// Returns a list of (user_id, amount_credited) for logging.
pub fn distribute_reward(
    db:           &Db,
    miners:       &[(String, u64)],   // (user_id, hashrate)
    block_reward: f64,
    block_index:  u64,
) -> Vec<(String, f64)> {
    let total_hr: u64 = miners.iter().map(|(_, hr)| hr).sum();
    if total_hr == 0 || miners.is_empty() { return vec![]; }

    let mut credited = vec![];

    for (user_id, hashrate) in miners {
        if *hashrate == 0 { continue; }
        let share  = *hashrate as f64 / total_hr as f64;
        let amount = (block_reward * share * 10000.0).round() / 10000.0; // 4 decimal places
        if amount <= 0.0 { continue; }

        let memo = format!(
            "Mining reward block #{} ({:.1}% hashrate share)",
            block_index,
            share * 100.0
        );

        if credit(db, user_id, amount, &memo).is_ok() {
            credited.push((user_id.clone(), amount));
        }
    }

    credited
}
