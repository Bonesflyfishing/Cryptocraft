// ─── auth.rs ──────────────────────────────────────────────────────────────────
// SQLite-backed authentication.
//
// The database lives in a single file: cryptocraft_users.db
// It has one table:
//
//   CREATE TABLE users (
//       user_id       TEXT PRIMARY KEY,   -- UUID
//       email         TEXT UNIQUE,        -- lowercase, trimmed
//       password_hash TEXT,               -- Argon2id hash
//       created_at    INTEGER             -- Unix timestamp
//   )
//
// Passwords are never stored in plain text — only the Argon2id hash.
// The AuthProvider trait is unchanged so swapping to a server backend later
// still requires zero changes in main.rs.
// ─────────────────────────────────────────────────────────────────────────────

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use crossterm::{execute, style::{Color, ResetColor, SetForegroundColor}};
use rusqlite::{Connection, params};
use std::{
    io::{self, Write},
    time::{SystemTime, UNIX_EPOCH},
};

const DB_FILE: &str = "cryptocraft_users.db";

// ─── Session ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Session {
    pub email:      String,
    pub user_id:    String,
    pub chain_file: String,
}

// ─── AuthProvider trait ───────────────────────────────────────────────────────

pub trait AuthProvider {
    fn register(&mut self, email: &str, password: &str) -> Result<Session, String>;
    fn login(&mut self, email: &str, password: &str)    -> Result<Session, String>;
}

// ─── SQLite implementation ────────────────────────────────────────────────────

pub struct LocalAuthProvider {
    conn: Connection,
}

impl LocalAuthProvider {
    /// Open (or create) the SQLite database and make sure the users table exists.
    pub fn load() -> Self {
        let conn = Connection::open(DB_FILE)
            .expect("Failed to open SQLite database");

        // CREATE TABLE IF NOT EXISTS is safe to run every launch.
        // If the table already exists this is a no-op.
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS users (
                user_id       TEXT PRIMARY KEY,
                email         TEXT UNIQUE NOT NULL,
                password_hash TEXT NOT NULL,
                created_at    INTEGER NOT NULL
            );
        ").expect("Failed to create users table");

        LocalAuthProvider { conn }
    }

    /// Returns true if at least one user exists in the database.
    pub fn has_users(&self) -> bool {
        let count: i64 = self.conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .unwrap_or(0);
        count > 0
    }

    /// Turn an email into a safe filename for the chain JSON.
    fn chain_file_for(email: &str) -> String {
        let safe = email.replace(['@', '.', '+', ' '], "_");
        format!("cryptocraft_chain_{}.json", safe)
    }
}

impl AuthProvider for LocalAuthProvider {
    fn register(&mut self, email: &str, password: &str) -> Result<Session, String> {
        let email = email.trim().to_lowercase();

        // ── Validate ──────────────────────────────────────────────────────────
        if email.is_empty() || !email.contains('@') {
            return Err("Please enter a valid email address.".into());
        }
        if password.len() < 8 {
            return Err("Password must be at least 8 characters.".into());
        }

        // ── Check for duplicate email ─────────────────────────────────────────
        let exists: bool = self.conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE email = ?1",
                params![email],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .unwrap_or(false);

        if exists {
            return Err("An account with that email already exists.".into());
        }

        // ── Hash password with Argon2id ───────────────────────────────────────
        // A unique random salt per user means identical passwords produce
        // completely different hashes in the database.
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| e.to_string())?
            .to_string();

        // ── INSERT into SQLite ────────────────────────────────────────────────
        let user_id    = uuid::Uuid::new_v4().to_string();
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;

        self.conn.execute(
            "INSERT INTO users (user_id, email, password_hash, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![user_id, email, hash, created_at],
        ).map_err(|e| format!("Database error: {}", e))?;

        Ok(Session {
            chain_file: Self::chain_file_for(&email),
            email,
            user_id,
        })
    }

    fn login(&mut self, email: &str, password: &str) -> Result<Session, String> {
        let email = email.trim().to_lowercase();

        // ── SELECT user row ───────────────────────────────────────────────────
        // Returns Err(QueryReturnedNoRows) automatically if email not found.
        let (user_id, stored_hash): (String, String) = self.conn
            .query_row(
                "SELECT user_id, password_hash FROM users WHERE email = ?1",
                params![email],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| "No account found with that email.".to_string())?;

        // ── Verify password ───────────────────────────────────────────────────
        let parsed = PasswordHash::new(&stored_hash)
            .map_err(|e| e.to_string())?;

        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| "Incorrect password.".to_string())?;

        Ok(Session {
            chain_file: Self::chain_file_for(&email),
            email,
            user_id,
        })
    }
}

// ─── Interactive terminal login flow ──────────────────────────────────────────
// Nothing below here is storage-related. UI is fully separate from the database.

fn prompt(label: &str) -> String {
    print!("  {}: ", label);
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
    buf.trim().to_string()
}

fn prompt_password(label: &str) -> String {
    print!("  {}: ", label);
    io::stdout().flush().ok();
    rpassword::read_password().unwrap_or_default()
}

fn print_auth_header(new_user: bool) {
    let mut out = io::stdout();
    execute!(out, SetForegroundColor(Color::Yellow)).ok();
    if new_user {
        println!("  ╔══════════════════════════════════════╗");
        println!("  ║   Create your CryptoCraft account    ║");
        println!("  ╚══════════════════════════════════════╝");
    } else {
        println!("  ╔══════════════════════════════════════╗");
        println!("  ║   Welcome back — please log in       ║");
        println!("  ╚══════════════════════════════════════╝");
    }
    execute!(out, ResetColor).ok();
    println!();
}

pub fn run_auth_flow(auth: &mut dyn AuthProvider, has_existing_users: bool) -> Session {
    loop {
        println!();
        execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
        if has_existing_users {
            println!("  [1] Login        [2] Register");
        } else {
            println!("  No accounts found. Let's create one!");
        }
        execute!(io::stdout(), ResetColor).ok();
        println!();

        let choice = if has_existing_users {
            prompt("Choice (1/2)")
        } else {
            "2".to_string()
        };

        match choice.trim() {
            "1" => {
                print_auth_header(false);
                let email    = prompt("Email");
                let password = prompt_password("Password");

                match auth.login(&email, &password) {
                    Ok(session) => {
                        execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
                        println!("\n  Logged in as {}. Welcome back!\n", session.email);
                        execute!(io::stdout(), ResetColor).ok();
                        std::thread::sleep(std::time::Duration::from_millis(600));
                        return session;
                    }
                    Err(e) => {
                        execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
                        println!("\n  Error: {}\n", e);
                        execute!(io::stdout(), ResetColor).ok();
                    }
                }
            }

            _ => {
                print_auth_header(true);
                let email    = prompt("Email");
                let password = prompt_password("Password (min 8 chars)");
                let confirm  = prompt_password("Confirm password");

                if password != confirm {
                    execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
                    println!("\n  Passwords do not match.\n");
                    execute!(io::stdout(), ResetColor).ok();
                    continue;
                }

                match auth.register(&email, &password) {
                    Ok(session) => {
                        execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
                        println!("\n  Account created! Welcome, {}!\n", session.email);
                        execute!(io::stdout(), ResetColor).ok();
                        std::thread::sleep(std::time::Duration::from_millis(600));
                        return session;
                    }
                    Err(e) => {
                        execute!(io::stdout(), SetForegroundColor(Color::Red)).ok();
                        println!("\n  Error: {}\n", e);
                        execute!(io::stdout(), ResetColor).ok();
                    }
                }
            }
        }
    }
}
