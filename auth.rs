// ─── auth.rs ──────────────────────────────────────────────────────────────────
// Local disk authentication with a server-ready interface.
//
// When you're ready to add a backend, implement the `AuthProvider` trait for
// your HTTP client and swap `LocalAuthProvider` out in main.rs. The rest of
// the codebase doesn't need to change.
// ──────────────────────────────────────────────────────────────────────────────

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use crossterm::{
    execute,
    style::{Color, ResetColor, SetForegroundColor},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::Path,
};

const USERS_FILE: &str = "cryptocraft_users.json";

// ─── Session (returned on successful login) ───────────────────────────────────

#[derive(Debug, Clone)]
pub struct Session {
    pub email:      String,
    pub user_id:    String,
    pub chain_file: String,  // unique save file per user
}

// ─── AuthProvider trait (swap this for HTTP later) ───────────────────────────

pub trait AuthProvider {
    /// Register a new user. Returns Err if email already taken.
    fn register(&mut self, email: &str, password: &str) -> Result<Session, String>;
    /// Login an existing user. Returns Err if credentials wrong.
    fn login(&mut self, email: &str, password: &str) -> Result<Session, String>;
}

// ─── Stored user record ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserRecord {
    user_id:       String,
    email:         String,
    password_hash: String,
}

// ─── Local disk implementation ────────────────────────────────────────────────

pub struct LocalAuthProvider {
    users: HashMap<String, UserRecord>, // keyed by email
}

impl LocalAuthProvider {
    pub fn load() -> Self {
        let users = if Path::new(USERS_FILE).exists() {
            fs::read_to_string(USERS_FILE)
                .ok()
                .and_then(|d| serde_json::from_str::<Vec<UserRecord>>(&d).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|u| (u.email.clone(), u))
                .collect()
        } else {
            HashMap::new()
        };
        LocalAuthProvider { users }
    }

    fn save(&self) {
        let records: Vec<&UserRecord> = self.users.values().collect();
        if let Ok(json) = serde_json::to_string_pretty(&records) {
            let _ = fs::write(USERS_FILE, json);
        }
    }

    pub fn has_users(&self) -> bool { !self.users.is_empty() }

    fn make_session(record: &UserRecord) -> Session {
        // Sanitise email for use as a filename
        let safe = record.email.replace(['@', '.', '+'], "_");
        Session {
            email:      record.email.clone(),
            user_id:    record.user_id.clone(),
            chain_file: format!("cryptocraft_chain_{}.json", safe),
        }
    }
}

impl AuthProvider for LocalAuthProvider {
    fn register(&mut self, email: &str, password: &str) -> Result<Session, String> {
        let email = email.trim().to_lowercase();

        if self.users.contains_key(&email) {
            return Err("An account with that email already exists.".to_string());
        }
        if email.is_empty() || !email.contains('@') {
            return Err("Please enter a valid email address.".to_string());
        }
        if password.len() < 8 {
            return Err("Password must be at least 8 characters.".to_string());
        }

        let salt        = SaltString::generate(&mut OsRng);
        let argon2      = Argon2::default();
        let hash        = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| e.to_string())?
            .to_string();

        let user_id = uuid::Uuid::new_v4().to_string();
        let record  = UserRecord { user_id, email: email.clone(), password_hash: hash };
        let session = Self::make_session(&record);
        self.users.insert(email, record);
        self.save();
        Ok(session)
    }

    fn login(&mut self, email: &str, password: &str) -> Result<Session, String> {
        let email = email.trim().to_lowercase();

        let record = self.users
            .get(&email)
            .ok_or_else(|| "No account found with that email.".to_string())?;

        let parsed = PasswordHash::new(&record.password_hash)
            .map_err(|e| e.to_string())?;

        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| "Incorrect password.".to_string())?;

        Ok(Self::make_session(record))
    }
}

// ─── Interactive terminal login flow ─────────────────────────────────────────

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

/// Full interactive auth flow. Returns a Session when the user is authenticated.
/// `auth` is your AuthProvider — swap to a server implementation here later.
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
            // ── Login ──
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

            // ── Register ──
            "2" | _ => {
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
                        println!("\n  Account created! Welcome to CryptoCraft, {}!\n", session.email);
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