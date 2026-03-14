// ─── blockchain.rs ────────────────────────────────────────────────────────────
// Shared blockchain types used by solo miner, pool server, and pool client.
// ─────────────────────────────────────────────────────────────────────────────

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, time::{SystemTime, UNIX_EPOCH}};

pub const BLOCK_REWARD: f64               = 12.0;
pub const HALVING_INTERVAL: u64           = 100;
pub const TARGET_BLOCK_TIME_SECS: f64     = 120.0;  // target 2 minutes per block
pub const DIFFICULTY_ADJUST_INTERVAL: u64 = 10;
pub const MAX_DIFFICULTY: usize           = 16;
pub const MIN_DIFFICULTY: usize           = 1;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

// ── CPU benchmark ─────────────────────────────────────────────────────────────
// Hashes as fast as possible for `duration_secs` seconds across all cores,
// then returns the measured hashrate and the recommended starting difficulty.
//
// Formula: difficulty = floor(log16(hashrate * target_block_time))
// Clamped to [MIN_DIFFICULTY, MAX_DIFFICULTY].
pub fn benchmark_difficulty(duration_secs: f64) -> (u64, usize) {
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    let counter   = std::sync::Arc::new(AtomicU64::new(0));
    let stop      = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let n_threads = num_cpus::get().max(1);
    let start     = Instant::now();

    // Signal threads to stop after duration
    {
        let s = stop.clone();
        let dur = std::time::Duration::from_secs_f64(duration_secs);
        std::thread::spawn(move || {
            std::thread::sleep(dur);
            s.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    }

    (0..n_threads).into_par_iter().for_each(|t| {
        let c = counter.clone();
        let s = stop.clone();
        let mut nonce: u64 = t as u64 * (u64::MAX / n_threads as u64);
        let mut local = 0u64;
        while !s.load(Ordering::Relaxed) {
            // Just hash — content doesn't matter for benchmarking
            let raw = format!("benchmark{}", nonce);
            let mut h = Sha256::new();
            h.update(raw.as_bytes());
            let _ = hex::encode(h.finalize());
            local += 1;
            if local % 5_000 == 0 {
                c.fetch_add(5_000, Ordering::Relaxed);
                local = 0;
            }
            nonce = nonce.wrapping_add(1);
        }
    });

    let elapsed  = start.elapsed().as_secs_f64().max(0.1);
    let hashrate = (counter.load(std::sync::atomic::Ordering::Relaxed) as f64 / elapsed) as u64;

    // log16(hashrate * target_time) = log2(hashrate * target_time) / log2(16) = / 4
    let ideal = (hashrate as f64 * TARGET_BLOCK_TIME_SECS).log2() / 4.0;
    let diff  = (ideal.floor() as usize).clamp(MIN_DIFFICULTY, MAX_DIFFICULTY);

    (hashrate, diff)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub index:         u64,
    pub timestamp:     u64,
    pub data:          String,
    pub previous_hash: String,
    pub hash:          String,
    pub nonce:         u64,
    pub difficulty:    usize,
    pub miner:         String,
    pub reward:        f64,
}

impl Block {
    pub fn genesis() -> Self {
        let mut b = Block {
            index: 0,
            timestamp: now_secs(),
            data: "Genesis Block -- CryptoCraft v1.0.0".to_string(),
            previous_hash: "0".repeat(64),
            hash: String::new(),
            nonce: 0,
            difficulty: 1,
            miner: "GENESIS".to_string(),
            reward: 0.0,
        };
        b.hash = b.compute_hash(0);
        b
    }

    pub fn compute_hash(&self, nonce: u64) -> String {
        let raw = format!(
            "{}{}{}{}{}{}",
            self.index, self.timestamp, self.data,
            self.previous_hash, nonce, self.difficulty
        );
        let mut h = Sha256::new();
        h.update(raw.as_bytes());
        hex::encode(h.finalize())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Blockchain {
    pub chain:       Vec<Block>,
    pub difficulty:  usize,
    pub total_mined: f64,
    pub miner_name:  String,
}

impl Blockchain {
    pub fn new(miner_name: &str) -> Self {
        Blockchain {
            chain: vec![Block::genesis()],
            difficulty: MIN_DIFFICULTY,
            total_mined: 0.0,
            miner_name: miner_name.to_string(),
        }
    }

    pub fn load_or_new(miner_name: &str, save_file: &str) -> Self {
        if Path::new(save_file).exists() {
            if let Ok(data) = fs::read_to_string(save_file) {
                if let Ok(mut bc) = serde_json::from_str::<Blockchain>(&data) {
                    bc.miner_name = miner_name.to_string();
                    return bc;
                }
            }
        }
        Blockchain::new(miner_name)
    }

    pub fn save(&self, save_file: &str) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(save_file, json);
        }
    }

    pub fn latest_hash(&self) -> &str { &self.chain.last().unwrap().hash }
    pub fn next_index(&self) -> u64   { self.chain.len() as u64 }

    pub fn current_reward(&self) -> f64 {
        let halvings = self.next_index() / HALVING_INTERVAL;
        BLOCK_REWARD / 2f64.powi(halvings as i32)
    }

    pub fn add_block(&mut self, nonce: u64, hash: String, attempts: u64) -> Block {
        let index  = self.next_index();
        let reward = self.current_reward();
        let block  = Block {
            index,
            timestamp: now_secs(),
            data: format!("Block #{} mined after {} attempts", index, attempts),
            previous_hash: self.latest_hash().to_string(),
            hash, nonce,
            difficulty: self.difficulty,
            miner: self.miner_name.clone(),
            reward,
        };
        self.total_mined += reward;
        self.chain.push(block.clone());
        self.adjust_difficulty();
        block
    }

    pub fn adjust_difficulty(&mut self) {
        let len = self.chain.len() as u64;
        if len < DIFFICULTY_ADJUST_INTERVAL || len % DIFFICULTY_ADJUST_INTERVAL != 0 { return; }
        let window  = &self.chain[self.chain.len() - DIFFICULTY_ADJUST_INTERVAL as usize..];
        let elapsed = window.last().unwrap().timestamp - window.first().unwrap().timestamp;
        let target  = TARGET_BLOCK_TIME_SECS as u64 * (DIFFICULTY_ADJUST_INTERVAL - 1);
        if elapsed < target / 2 {
            self.difficulty = (self.difficulty + 1).min(MAX_DIFFICULTY);
        } else if elapsed > target * 2 {
            self.difficulty = self.difficulty.saturating_sub(1).max(MIN_DIFFICULTY);
        }
    }
}
