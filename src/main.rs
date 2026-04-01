mod auth;
mod blockchain;
mod db;
mod network;
mod pool_client;
mod pool_server;
mod server;
mod sync;
mod wallet;

use auth::run_auth_flow;
use blockchain::*;
use crossterm::{
    cursor, execute,
    event::{self, Event, KeyCode, KeyEventKind},
    style::{Color, ResetColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use rand::Rng;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    io::{self, BufRead, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use serde::{Deserialize, Serialize};

const VERSION: &str = "1.0.0";

// ---- Banner ------------------------------------------------------------------

fn print_banner() {
    let mut out = io::stdout();
    execute!(out, SetForegroundColor(Color::Yellow)).ok();
    writeln!(out, r"
   ___  ____  _  _  ____  ____  ____  ___  ____   __   ____  ____
  / __)(  _ \( \/ )(  _ \(_  _)( ___)(__ \(  _ \ / _\ (  __)(_  _)
 ( (__  )   / \  /  )___/  )(   )__)  / _/ )   //    \ ) _)   )(
  \___)(__\_) (__) (__)   (__) (____) (__)(____/ \_/\_/(__)   (__)
    ").ok();
    execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
    writeln!(out, "       Proof-of-Work Blockchain Mining Engine  |  v{}", VERSION).ok();
    execute!(out, ResetColor).ok();
    writeln!(out).ok();
    out.flush().ok();
}

fn clear_screen() {
    execute!(io::stdout(), terminal::Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
}

// ---- Mode selector -----------------------------------------------------------

fn pick_mode() -> u8 {
    loop {
        execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
        println!("  +----------------------------------+");
        println!("  |   Select Mode                    |");
        println!("  +----------------------------------+");
        execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
        println!("  [1] Solo Mine         (this machine only)");
        println!("  [2] Host Mining Pool  (this machine is the server)");
        println!("  [3] Join Mining Pool  (connect to pool server)");
        println!("  [4] Wallet            (balance, send CC, history)");
        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("  [5] Exit");
        execute!(io::stdout(), ResetColor).ok();
        println!();
        print!("  Choice: ");
        io::stdout().flush().ok();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).ok();
        match buf.trim() {
            "1" => return 1,
            "2" => return 2,
            "3" => return 3,
            "4" => return 4,
            "5" => return 5,
            _   => println!("  Please enter 1–5.\n"),
        }
    }
}

// ---- Main --------------------------------------------------------------------

fn main() {
    clear_screen();
    print_banner();

    // Open shared SQLite database — used by auth, wallet, and pool server
    let database = db::open();

    // Auth — only happens once at startup
    let mut auth_provider = auth::LocalAuthProvider::new(database.clone());
    let has_users         = auth_provider.has_users();
    let session           = run_auth_flow(&mut auth_provider, has_users);

    let miner_name = session.email
        .split('@').next().unwrap_or("miner").to_string();

    // ── Main menu loop ────────────────────────────────────────────────────────
    loop {
        clear_screen();
        print_banner();

        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("  Logged in as: {}\n", session.email);
        execute!(io::stdout(), ResetColor).ok();

        let mode = pick_mode();
        clear_screen();

        match mode {
            // ── Solo mining ───────────────────────────────────────────────────
            1 => {
                print_banner();
                println!("  Loading chain for {}...", miner_name);
                std::thread::sleep(Duration::from_millis(400));

                let mut blockchain = Blockchain::load_or_new(&miner_name, &session.chain_file);

                // ── CPU benchmark on fresh chain only ─────────────────────────
                // Only runs when chain has just the genesis block — skipped on
                // all subsequent launches since difficulty is already calibrated.
                if blockchain.chain.len() == 1 {
                    clear_screen();
                    print_banner();
                    execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
                    println!("  New chain detected — running CPU benchmark...");
                    println!("  This takes 3 seconds and only happens once.");
                    println!();
                    execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
                    println!("  Detecting: {} logical cores", num_cpus::get());
                    execute!(io::stdout(), ResetColor).ok();
                    io::stdout().flush().ok();

                    let (hashrate, diff) = benchmark_difficulty(3.0);

                    execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
                    println!();
                    println!("  Measured hashrate : {}", fmt_hashrate(hashrate as f64));
                    println!("  Starting difficulty: {} leading zeros", diff);
                    println!();
                    execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
                    println!("  (Targeting ~{} minutes per block)", TARGET_BLOCK_TIME_SECS as u64 / 60);
                    execute!(io::stdout(), ResetColor).ok();

                    blockchain.difficulty = diff;
                    std::thread::sleep(Duration::from_millis(1500));
                }

                println!(
                    "  Chain: {} blocks  |  Difficulty: {}  |  Earned: {:.4} CC",
                    blockchain.chain.len(), blockchain.difficulty, blockchain.total_mined
                );

                // ── Silent server sync ────────────────────────────────────────
                // Try to find and sync any offline-mined rewards to the server.
                // Spawned in background so startup is never delayed.
                {
                    let email_c      = session.email.clone();
                    let chain_file_c = session.chain_file.clone();
                    let miner_name_c = miner_name.clone();
                    std::thread::spawn(move || {
                        let mut bc = Blockchain::load_or_new(&miner_name_c, &chain_file_c);
                        sync::try_sync(&mut bc, &email_c, &chain_file_c);
                    });
                }

                let dashboard_html = include_str!("../dashboard.html")
                    .replace("// __SERVER_MODE__", "window.__SERVER_MODE__ = true;");
                let srv_state = server::ServerState::new(&session.chain_file);
                server::spawn(srv_state.clone(), dashboard_html);
                let ip = server::local_ip();
                execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
                println!("  Dashboard : http://{}:{}/", ip, server::PORT);
                execute!(io::stdout(), ResetColor).ok();
                std::thread::sleep(Duration::from_millis(800));
                clear_screen();

                run_solo_miner(blockchain, session.chain_file.clone(), miner_name.clone(), session.email.clone(), ip, srv_state, database.clone());
            }

            // ── Pool server ───────────────────────────────────────────────────
            2 => {
                println!("  Loading chain for pool server...");
                std::thread::sleep(Duration::from_millis(400));
                let mut blockchain = Blockchain::load_or_new(&miner_name, &session.chain_file);
                clear_screen();

                // Benchmark on fresh chain only
                if blockchain.chain.len() == 1 {
                    print_banner();
                    execute!(io::stdout(), SetForegroundColor(Color::Cyan)).ok();
                    println!("  New chain — running CPU benchmark (3s)...");
                    execute!(io::stdout(), ResetColor).ok();
                    let (hashrate, diff) = benchmark_difficulty(3.0);
                    execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
                    println!("  Hashrate: {}  →  Starting difficulty: {} zeros",
                        fmt_hashrate(hashrate as f64), diff);
                    execute!(io::stdout(), ResetColor).ok();
                    blockchain.difficulty = diff;
                    std::thread::sleep(Duration::from_millis(1200));
                    clear_screen();
                }

                let (bind_ip, display_ip) = network::pick_host_interface();
                clear_screen();
                execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
                println!("  Starting pool server...");
                println!("  Bound to    : {}:{}", display_ip, pool_server::POOL_PORT);
                println!("  Clients use : {}:{}", display_ip, pool_server::POOL_PORT);
                execute!(io::stdout(), ResetColor).ok();
                std::thread::sleep(Duration::from_millis(800));
                clear_screen();

                pool_server::run(blockchain, session.chain_file.clone(), bind_ip, database.clone());
            }

            // ── Pool client ───────────────────────────────────────────────────
            3 => {
                clear_screen();
                let server_addr = network::pick_server_address(pool_server::POOL_PORT);
                clear_screen();
                pool_client::run(session.email.clone(), miner_name.clone(), server_addr);
            }

            // ── Wallet ────────────────────────────────────────────────────────
            4 => {
                wallet::run(&database, &session);
            }

            // ── Exit ─────────────────────────────────────────────────────────
            5 | _ => {
                clear_screen();
                execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
                println!("\n  Goodbye, {}!\n", miner_name);
                execute!(io::stdout(), ResetColor).ok();
                break;
            }
        }

        // Brief pause and confirmation before going back to menu
        execute!(io::stdout(), SetForegroundColor(Color::DarkGrey)).ok();
        println!("\n  Returning to main menu...");
        execute!(io::stdout(), ResetColor).ok();
        std::thread::sleep(Duration::from_millis(800));
    }
}

// ---- Solo miner (unchanged from before) --------------------------------------

fn run_solo_miner(
    mut blockchain: Blockchain,
    chain_file:     String,
    miner_name:     String,
    email:          String,
    local_ip:       String,
    srv_state:      server::ServerState,
    database:       db::Db,
) {
    let user_quit    = Arc::new(AtomicBool::new(false));
    let mine_stop    = Arc::new(AtomicBool::new(false));
    let hash_counter = Arc::new(AtomicU64::new(0));
    let peek_hash    = Arc::new(Mutex::new("0".repeat(16)));

    let mut ui           = Ui::new();
    let mut blocks_found = 0u64;
    let mut flavor       = random_flavor();
    let mut flavor_t     = Instant::now();
    let mut last_hr_push = Instant::now();

    'outer: while !user_quit.load(Ordering::Relaxed) {
        let template = Block {
            index:         blockchain.next_index(),
            timestamp:     now_secs(),
            data:          format!("CryptoCraft Block #{}", blockchain.next_index()),
            previous_hash: blockchain.latest_hash().to_string(),
            hash:          String::new(),
            nonce:         0,
            difficulty:    blockchain.difficulty,
            miner:         miner_name.clone(),
            reward:        blockchain.current_reward(),
        };

        let difficulty = blockchain.difficulty;
        mine_stop.store(false, Ordering::SeqCst);

        let ms  = mine_stop.clone();
        let hc  = hash_counter.clone();
        let ph  = peek_hash.clone();
        let tmp = template.clone();

        let handle = std::thread::spawn(move || mine_parallel(&tmp, difficulty, ms, hc, ph));

        // Enable raw mode so we can read keypresses without Enter
        let _ = terminal::enable_raw_mode();

        loop {
            if flavor_t.elapsed() > Duration::from_secs(4) {
                flavor   = random_flavor();
                flavor_t = Instant::now();
            }
            let hashes = hash_counter.load(Ordering::Relaxed);
            let peek   = peek_hash.lock().map(|p| p.clone()).unwrap_or_default();
            ui.draw(&blockchain, hashes, &peek, flavor, blocks_found, &email, &local_ip);

            // Push live hashrate to dashboard every 3 seconds
            if last_hr_push.elapsed().as_secs() >= 3 {
                let rhr = ui.recent_hashrate(hashes) as u64;
                srv_state.set_hashrate(rhr);
                last_hr_push = Instant::now();
            }

            // Poll for Q keypress or resize — non-blocking, 100ms timeout
            if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                match event::read() {
                    Ok(Event::Key(key)) => {
                        if key.kind == KeyEventKind::Press {
                            if key.code == KeyCode::Char('q') || key.code == KeyCode::Char('Q') {
                                let _ = terminal::disable_raw_mode();
                                user_quit.store(true, Ordering::SeqCst);
                                mine_stop.store(true, Ordering::SeqCst);
                                let _ = handle.join();
                                break 'outer;
                            }
                        }
                    }
                    Ok(Event::Resize(_, _)) => {
                        execute!(io::stdout(),
                            terminal::Clear(ClearType::All),
                            cursor::MoveTo(0, 0)
                        ).ok();
                    }
                    _ => {}
                }
            }

            if user_quit.load(Ordering::Relaxed) {
                let _ = terminal::disable_raw_mode();
                mine_stop.store(true, Ordering::SeqCst);
                let _ = handle.join();
                break 'outer;
            }
            if handle.is_finished() {
                let _ = terminal::disable_raw_mode();
                break;
            }
        }

        if user_quit.load(Ordering::Relaxed) { break; }

        match handle.join().ok().flatten() {
            Some((nonce, hash)) => {
                let attempts = hash_counter.load(Ordering::Relaxed);
                hash_counter.store(0, Ordering::Relaxed); // reset for next block
                let block    = blockchain.add_block(nonce, hash, attempts);
                blockchain.save(&chain_file);
                blocks_found += 1;

                // Credit reward to wallet
                if let Some(uid) = db::user_id_for_email(&database, &email) {
                    let memo = format!("Solo mining reward — block #{}", block.index);
                    db::credit(&database, &uid, block.reward, &memo).ok();
                }

                print_found_block(&block);
                clear_screen();
            }
            None => break,
        }
    }

    // Shutdown
    clear_screen();
    let total_hashes = hash_counter.load(Ordering::Relaxed);
    execute!(io::stdout(), SetForegroundColor(Color::Yellow)).ok();
    println!("\n  Mining session complete.\n");
    execute!(io::stdout(), SetForegroundColor(Color::Green)).ok();
    println!("  Blocks Found : {}", blocks_found);
    println!("  Total Hashes : {}", total_hashes);
    println!("  CC Earned    : {:.4} CC", blockchain.total_mined);
    println!("  Chain saved  : {}", chain_file);
    execute!(io::stdout(), ResetColor).ok();
    println!();
}

// ---- Mining core (solo) -------------------------------------------------------

fn mine_parallel(
    template:     &Block,
    difficulty:   usize,
    stop:         Arc<AtomicBool>,
    hash_counter: Arc<AtomicU64>,
    peek_hash:    Arc<Mutex<String>>,
) -> Option<(u64, String)> {
    let prefix    = "0".repeat(difficulty);
    let n_threads = num_cpus::get().max(1);
    let chunk     = u64::MAX / n_threads as u64;
    let result    = Arc::new(Mutex::new(None::<(u64, String)>));

    (0..n_threads).into_par_iter().for_each(|t| {
        let start     = t as u64 * chunk;
        let end       = if t == n_threads - 1 { u64::MAX } else { start + chunk };
        let stop_r    = stop.clone();
        let counter_r = hash_counter.clone();
        let peek_r    = peek_hash.clone();
        let result_r  = result.clone();
        let prefix_r  = prefix.clone();
        let mut local = 0u64;

        for nonce in start..=end {
            if stop_r.load(Ordering::Relaxed) { return; }
            if result_r.lock().unwrap().is_some() { return; }
            let hash  = template.compute_hash(nonce);
            local    += 1;
            if local % 10_000 == 0 {
                counter_r.fetch_add(10_000, Ordering::Relaxed);
                if let Ok(mut p) = peek_r.try_lock() { *p = hash[..16].to_string(); }
                local = 0;
            }
            if hash.starts_with(prefix_r.as_str()) {
                *result_r.lock().unwrap() = Some((nonce, hash));
                stop_r.store(true, Ordering::Relaxed);
                return;
            }
        }
    });

    Arc::try_unwrap(result).ok()?.into_inner().ok()?
}

// ---- UI (solo) ---------------------------------------------------------------

fn difficulty_bar(d: usize) -> String {
    format!("[{}{}]", "#".repeat(d), ".".repeat(MAX_DIFFICULTY.saturating_sub(d)))
}

fn fmt_hashrate(hr: f64) -> String {
    if hr >= 1_000_000.0  { format!("{:.2} MH/s", hr / 1_000_000.0) }
    else if hr >= 1_000.0 { format!("{:.2} KH/s", hr / 1_000.0) }
    else                  { format!("{:.2}  H/s", hr) }
}

fn fmt_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0      { format!("{}h {}m {}s", h, m, s) }
    else if m > 0 { format!("{}m {}s", m, s) }
    else          { format!("{}s", s) }
}

fn leading_zeros(hash: &str) -> usize {
    hash.chars().take_while(|&c| c == '0').count()
}

static FLAVORS: &[&str] = &[
    "Hammering nonces at full speed...",
    "Crafting the perfect hash...",
    "Burning CPU cycles for glory...",
    "Hunting for leading zeros...",
    "Deep mining in progress...",
    "Building the chain, one block at a time...",
    "Every zero gets us closer...",
    "The blockchain never sleeps...",
    "Proof-of-work at its finest...",
    "SHA-256 at maximum overdrive...",
];

fn random_flavor() -> &'static str {
    FLAVORS[rand::thread_rng().gen_range(0..FLAVORS.len())]
}

struct Ui {
    start:            Instant,
    hashrate_history: VecDeque<(Instant, u64)>,
}

impl Ui {
    fn new() -> Self { Ui { start: Instant::now(), hashrate_history: VecDeque::with_capacity(15) } }

    fn recent_hashrate(&mut self, hashes: u64) -> f64 {
        let now = Instant::now();
        self.hashrate_history.push_back((now, hashes));
        if self.hashrate_history.len() > 12 { self.hashrate_history.pop_front(); }
        if self.hashrate_history.len() < 2  { return 0.0; }
        let (t0, h0) = self.hashrate_history.front().unwrap();
        let (t1, h1) = self.hashrate_history.back().unwrap();
        let dt = t1.duration_since(*t0).as_secs_f64();
        if dt > 0.0 { (h1 - h0) as f64 / dt } else { 0.0 }
    }

    fn avg_hashrate(&self, hashes: u64) -> f64 {
        let e = self.start.elapsed().as_secs_f64();
        if e > 0.0 { hashes as f64 / e } else { 0.0 }
    }

    fn draw(&mut self, bc: &Blockchain, total_hashes: u64, peek: &str, flavor: &str, blocks_found: u64, email: &str, local_ip: &str) {
        let mut out    = io::stdout();
        let up         = self.start.elapsed().as_secs();
        let rhr        = self.recent_hashrate(total_hashes);
        let ahr        = self.avg_hashrate(total_hashes);
        let d          = bc.difficulty;
        let halving_in = HALVING_INTERVAL - (bc.next_index() % HALVING_INTERVAL);

        execute!(out, cursor::MoveTo(0, 0)).ok();

        execute!(out, SetForegroundColor(Color::Yellow)).ok();
        writeln!(out, "+------------------------------------------------------------------+").ok();
        writeln!(out, "|    CRYPTOCRAFT Solo Miner  v{:<5}                              |", VERSION).ok();
        writeln!(out, "+------------------------------------------------------------------+").ok();
        execute!(out, ResetColor).ok();

        execute!(out, SetForegroundColor(Color::Cyan)).ok();
        writeln!(out, "  Account : {:<28}  Uptime : {}", email, fmt_duration(up)).ok();
        writeln!(out, "  Miner   : {}", bc.miner_name).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::Green)).ok();
        writeln!(out, "  Blocks Mined  : {:<10}  Total Earned  : {:.4} CC", blocks_found, bc.total_mined).ok();
        writeln!(out, "  Block Reward  : {:.4} CC    Halving In    : {} blocks", bc.current_reward(), halving_in).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::Magenta)).ok();
        write!(out, "  Difficulty    : {} leading zeros  ", d).ok();
        execute!(out, SetForegroundColor(Color::Red)).ok();
        writeln!(out, "{}", difficulty_bar(d)).ok();
        execute!(out, ResetColor).ok();

        execute!(out, SetForegroundColor(Color::Yellow)).ok();
        writeln!(out, "  Avg Hashrate  : {:<20}  Recent : {}", fmt_hashrate(ahr), fmt_hashrate(rhr)).ok();
        writeln!(out, "  Total Hashes  : {}", total_hashes).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        let target_str = format!("{}{}", "0".repeat(d), "x".repeat(64 - d));
        execute!(out, SetForegroundColor(Color::Cyan)).ok();
        writeln!(out, "  Target        : {}", target_str).ok();
        let hit = peek.len().min(d);
        write!(out, "  Current Peek  : ").ok();
        execute!(out, SetForegroundColor(Color::Green)).ok();
        write!(out, "{}", &peek[..hit]).ok();
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "{:<64}", &peek[hit..]).ok();
        execute!(out, ResetColor).ok();

        execute!(out, SetForegroundColor(Color::White)).ok();
        writeln!(out, "  Status        : {:<58}", flavor).ok();
        execute!(out, ResetColor).ok();

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::Green)).ok();
        writeln!(out, "  Recent Blocks:").ok();
        execute!(out, ResetColor).ok();

        let recent: Vec<_> = bc.chain.iter().rev().skip(1).take(5).collect();
        if recent.is_empty() {
            for _ in 0..5 { writeln!(out, "{:70}", "").ok(); }
        } else {
            for blk in &recent {
                execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
                writeln!(out, "    #{:<5} | {}...{} | {} zeros | {:.4} CC",
                    blk.index, &blk.hash[..8], &blk.hash[56..], leading_zeros(&blk.hash), blk.reward).ok();
            }
            for _ in 0..(5usize.saturating_sub(recent.len())) { writeln!(out, "{:70}", "").ok(); }
            execute!(out, ResetColor).ok();
        }

        writeln!(out, "--------------------------------------------------------------------").ok();
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        writeln!(out, "  [Q + Enter] Back to menu                                          ").ok();
        execute!(out, SetForegroundColor(Color::Cyan)).ok();
        writeln!(out, "  Dashboard     : http://{}:{}/", local_ip, server::PORT).ok();
        execute!(out, ResetColor).ok();
        out.flush().ok();
    }
}

fn print_found_block(block: &Block) {
    let mut out = io::stdout();
    clear_screen();
    execute!(out, SetForegroundColor(Color::Yellow)).ok();
    writeln!(out, "\n\n  +---------------------------------------------------+").ok();
    writeln!(out,     "  |        BLOCK FOUND!  New block mined!            |").ok();
    writeln!(out,     "  +---------------------------------------------------+\n").ok();
    execute!(out, SetForegroundColor(Color::Green)).ok();
    writeln!(out, "  Block Index   : #{}", block.index).ok();
    writeln!(out, "  Nonce         : {}", block.nonce).ok();
    writeln!(out, "  Hash          : {}", block.hash).ok();
    writeln!(out, "  Leading Zeros : {}", leading_zeros(&block.hash)).ok();
    writeln!(out, "  Reward        : {:.4} CC", block.reward).ok();
    writeln!(out, "  Difficulty    : {}", block.difficulty).ok();
    execute!(out, ResetColor).ok();
    out.flush().ok();
    std::thread::sleep(Duration::from_millis(1200));
}
