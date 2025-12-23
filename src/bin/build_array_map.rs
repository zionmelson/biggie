use rayon::prelude::*;
/// Build complete tick/bin array map for TOP 150 pools by volume.
/// This generates 100% coverage for the most important pools.
///
/// Run: cargo run --release --bin build_array_map
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// Program IDs
const WHIRLPOOL_PROGRAM_ID: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const RAYDIUM_CLMM_PROGRAM_ID: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
const METEORA_DLMM_PROGRAM_ID: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

// SDK Constants
const WHIRLPOOL_TICK_ARRAY_SIZE: i32 = 88;
const WHIRLPOOL_MIN_TICK: i32 = -443636;
const WHIRLPOOL_MAX_TICK: i32 = 443636;

const RAYDIUM_TICK_ARRAY_SIZE: i32 = 60;
const RAYDIUM_MIN_TICK: i32 = -443636;
const RAYDIUM_MAX_TICK: i32 = 443636;

const METEORA_BIN_ARRAY_MIN_INDEX: i64 = -6656;
const METEORA_BIN_ARRAY_MAX_INDEX: i64 = 6655;

#[derive(Debug, Deserialize)]
struct PoolInfo {
    address: String,
    spacing: u16,
}

#[derive(Debug, Deserialize)]
struct TopPools {
    whirlpool: Vec<PoolInfo>,
    raydium: Vec<PoolInfo>,
    meteora: Vec<PoolInfo>,
}

#[derive(Debug, Serialize)]
struct ArrayEntry {
    t: String, // protocol type: "w", "r", "d"
    p: String, // pool address
    i: i64,    // start_index
    s: u16,    // spacing
}

fn derive_whirlpool_tick_array(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    let program_id = Pubkey::from_str(WHIRLPOOL_PROGRAM_ID).unwrap();
    let tick_str = start_tick_index.to_string();
    let seeds: &[&[u8]] = &[b"tick_array", pool.as_ref(), tick_str.as_bytes()];
    let (pda, _bump) = Pubkey::find_program_address(seeds, &program_id);
    pda
}

fn derive_raydium_tick_array(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    let program_id = Pubkey::from_str(RAYDIUM_CLMM_PROGRAM_ID).unwrap();
    let tick_bytes = start_tick_index.to_be_bytes();
    let seeds: &[&[u8]] = &[b"tick_array", pool.as_ref(), &tick_bytes];
    let (pda, _bump) = Pubkey::find_program_address(seeds, &program_id);
    pda
}

fn derive_meteora_bin_array(lb_pair: &Pubkey, bin_array_index: i64) -> Pubkey {
    let program_id = Pubkey::from_str(METEORA_DLMM_PROGRAM_ID).unwrap();
    let index_bytes = bin_array_index.to_le_bytes();
    let seeds: &[&[u8]] = &[b"bin_array", lb_pair.as_ref(), &index_bytes];
    let (pda, _bump) = Pubkey::find_program_address(seeds, &program_id);
    pda
}

fn process_whirlpool_pool(pool_info: &PoolInfo) -> Vec<(String, ArrayEntry)> {
    let pool = Pubkey::from_str(&pool_info.address).unwrap();
    let spacing = pool_info.spacing as i32;
    let array_width = spacing * WHIRLPOOL_TICK_ARRAY_SIZE;

    let min_start = -((WHIRLPOOL_MIN_TICK.abs() / array_width) * array_width);
    let max_start = (WHIRLPOOL_MAX_TICK / array_width) * array_width;

    let mut results = Vec::new();
    let mut start = min_start;
    while start <= max_start {
        let pda = derive_whirlpool_tick_array(&pool, start);
        results.push((
            pda.to_string(),
            ArrayEntry {
                t: "w".to_string(),
                p: pool_info.address.clone(),
                i: start as i64,
                s: pool_info.spacing,
            },
        ));
        start += array_width;
    }
    results
}

fn process_raydium_pool(pool_info: &PoolInfo) -> Vec<(String, ArrayEntry)> {
    let pool = Pubkey::from_str(&pool_info.address).unwrap();
    let spacing = pool_info.spacing as i32;
    let array_width = spacing * RAYDIUM_TICK_ARRAY_SIZE;

    let min_start = -((RAYDIUM_MIN_TICK.abs() / array_width) * array_width);
    let max_start = (RAYDIUM_MAX_TICK / array_width) * array_width;

    let mut results = Vec::new();
    let mut start = min_start;
    while start <= max_start {
        let pda = derive_raydium_tick_array(&pool, start);
        results.push((
            pda.to_string(),
            ArrayEntry {
                t: "r".to_string(),
                p: pool_info.address.clone(),
                i: start as i64,
                s: pool_info.spacing,
            },
        ));
        start += array_width;
    }
    results
}

fn process_meteora_pool(pool_info: &PoolInfo) -> Vec<(String, ArrayEntry)> {
    let lb_pair = Pubkey::from_str(&pool_info.address).unwrap();

    let mut results = Vec::new();
    for idx in METEORA_BIN_ARRAY_MIN_INDEX..=METEORA_BIN_ARRAY_MAX_INDEX {
        let pda = derive_meteora_bin_array(&lb_pair, idx);
        results.push((
            pda.to_string(),
            ArrayEntry {
                t: "d".to_string(),
                p: pool_info.address.clone(),
                i: idx,
                s: pool_info.spacing,
            },
        ));
    }
    results
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load the top 150 pools per protocol
    let pools_file = "/tmp/top_150_pools.json";
    println!("Loading pools from {}...", pools_file);

    let file = File::open(pools_file)?;
    let reader = BufReader::new(file);
    let pools: TopPools = serde_json::from_reader(reader)?;

    println!("Loaded pools:");
    println!("  Whirlpool: {}", pools.whirlpool.len());
    println!("  Raydium: {}", pools.raydium.len());
    println!("  Meteora: {}", pools.meteora.len());

    let mut result_map: HashMap<String, ArrayEntry> = HashMap::new();

    // Process Whirlpool pools in parallel
    println!("\nProcessing Whirlpool pools...");
    let counter = Arc::new(AtomicUsize::new(0));
    let wp_results: Vec<Vec<(String, ArrayEntry)>> = pools
        .whirlpool
        .par_iter()
        .map(|p| {
            let result = process_whirlpool_pool(p);
            let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
            if count % 10 == 0 {
                eprint!("\r  Processed {}/{} pools", count, pools.whirlpool.len());
            }
            result
        })
        .collect();
    eprintln!();

    for pool_results in wp_results {
        for (pda, entry) in pool_results {
            result_map.insert(pda, entry);
        }
    }
    println!("  Generated {} Whirlpool entries", result_map.len());
    let wp_count = result_map.len();

    // Process Raydium pools in parallel
    println!("\nProcessing Raydium pools...");
    let counter = Arc::new(AtomicUsize::new(0));
    let ray_results: Vec<Vec<(String, ArrayEntry)>> = pools
        .raydium
        .par_iter()
        .map(|p| {
            let result = process_raydium_pool(p);
            let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
            if count % 10 == 0 {
                eprint!("\r  Processed {}/{} pools", count, pools.raydium.len());
            }
            result
        })
        .collect();
    eprintln!();

    for pool_results in ray_results {
        for (pda, entry) in pool_results {
            result_map.insert(pda, entry);
        }
    }
    println!(
        "  Generated {} Raydium entries",
        result_map.len() - wp_count
    );
    let ray_count = result_map.len() - wp_count;

    // Process Meteora pools in parallel
    println!("\nProcessing Meteora pools...");
    let counter = Arc::new(AtomicUsize::new(0));
    let met_results: Vec<Vec<(String, ArrayEntry)>> = pools
        .meteora
        .par_iter()
        .map(|p| {
            let result = process_meteora_pool(p);
            let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
            if count % 10 == 0 {
                eprint!("\r  Processed {}/{} pools", count, pools.meteora.len());
            }
            result
        })
        .collect();
    eprintln!();

    for pool_results in met_results {
        for (pda, entry) in pool_results {
            result_map.insert(pda, entry);
        }
    }
    println!(
        "  Generated {} Meteora entries",
        result_map.len() - wp_count - ray_count
    );

    println!("\nTotal entries: {}", result_map.len());

    // Save to file
    let output_file = "clmm_array_map_top150.json";
    println!("\nSaving to {}...", output_file);
    let file = File::create(output_file)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer(writer, &result_map)?;

    let file_size = std::fs::metadata(output_file)?.len() as f64 / (1024.0 * 1024.0);
    println!("Done! File size: {:.1} MB", file_size);

    Ok(())
}
