//! Test CLMM and DLMM price calculation
//! 
//! Fetches multiple SOL/USDC pools across Raydium CLMM, Orca Whirlpool, and Meteora DLMM

use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

/// Raydium CLMM pool layout offsets
const RAYDIUM_LIQUIDITY_OFFSET: usize = 237;
const RAYDIUM_SQRT_PRICE_OFFSET: usize = 253;
const RAYDIUM_TICK_CURRENT_OFFSET: usize = 269;

/// Orca Whirlpool layout offsets (different from Raydium!)
const ORCA_SQRT_PRICE_OFFSET: usize = 65;      // sqrt_price_x64
const ORCA_TICK_CURRENT_OFFSET: usize = 261;   // tick_current_index
const ORCA_LIQUIDITY_OFFSET: usize = 265;      // liquidity

/// Meteora DLMM layout offsets (includes 8-byte Anchor discriminator)
/// Actual struct offsets are 68 and 72 after discriminator
const DLMM_ACTIVE_ID_OFFSET: usize = 8 + 68;   // i32 - active bin id (76)
const DLMM_BIN_STEP_OFFSET: usize = 8 + 72;    // u16 - bin step in basis points (80)

/// Q64 fixed point multiplier (2^64)
const Q64: u128 = 1u128 << 64;

fn main() -> Result<()> {
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    println!("=== CLMM Price Calculation Test ===\n");
    println!("RPC: {}\n", rpc_url);
    
    let rpc = RpcClient::new(rpc_url);
    
    // SOL/USDC Raydium CLMM pools from pools_old.json
    println!("--- Raydium CLMM SOL/USDC Pools ---\n");
    let raydium_pools = [
        ("2SjLv6XwViJ17rq21N1y98LbMee1J4DXinP61rk9v2aK", "Raydium CLMM #1"),
        ("3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv", "Raydium CLMM #2"),
        ("8sLbNZoA1cfnvMJLPfp98ZLAnFSYCFApfJKMbiXNLwxj", "Raydium CLMM #3"),
        ("CztrCcLhgfazkBchMW7wXQL37AWQdBP1tQWHBR249neh", "Raydium CLMM #4"),
    ];
    
    let mut raydium_prices = Vec::new();
    for (pool_addr_str, name) in raydium_pools {
        let pool_addr = Pubkey::from_str(pool_addr_str)?;
        
        match fetch_raydium_clmm_price(&rpc, &pool_addr, name, 9, 6) {
            Ok(price) => raydium_prices.push((name, price)),
            Err(e) => println!("  Error fetching {}: {}\n", name, e),
        }
    }
    
    // Orca Whirlpool SOL/USDC pools
    println!("\n--- Orca Whirlpool SOL/USDC Pools ---\n");
    let orca_pools = [
        ("HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ", "Orca Whirlpool #1"),
        ("Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE", "Orca Whirlpool #2"),
    ];
    
    let mut orca_prices = Vec::new();
    for (pool_addr_str, name) in orca_pools {
        let pool_addr = Pubkey::from_str(pool_addr_str)?;
        
        match fetch_orca_whirlpool_price(&rpc, &pool_addr, name, 9, 6) {
            Ok(price) => orca_prices.push((name, price)),
            Err(e) => println!("  Error fetching {}: {}\n", name, e),
        }
    }
    
    // Meteora DLMM SOL/USDC pools
    println!("\n--- Meteora DLMM SOL/USDC Pools ---\n");
    let dlmm_pools = [
        ("5rCf1DM8LjKTw4YqhnoLcngyZYeNnQqztScTogYHAS6", "Meteora DLMM #1"),
        ("8gJ7UWboMeQ6z6AQwFP3cAZwSYG8udVS2UesyCbH79r7", "Meteora DLMM #2"),
        ("CgqwPLSFfht89pF5RSKGUUMFj5zRxoUt4861w2SkXaqY", "Meteora DLMM #3"),
        ("9fjLVCpbHRHtGNyVGiuiq2jitSCc3vzyScAdYRtsRUe6", "Meteora DLMM #4"),
    ];
    
    let mut dlmm_prices = Vec::new();
    for (pool_addr_str, name) in dlmm_pools {
        let pool_addr = Pubkey::from_str(pool_addr_str)?;
        
        match fetch_dlmm_price(&rpc, &pool_addr, name, 9, 6) {
            Ok(price) => dlmm_prices.push((name, price)),
            Err(e) => println!("  Error fetching {}: {}\n", name, e),
        }
    }
    
    // Summary
    println!("\n=== Price Summary ===\n");
    println!("{:<25} {:>12}", "Pool", "Price (USD)");
    println!("{}", "-".repeat(40));
    for (name, price) in &raydium_prices {
        println!("{:<25} ${:>10.4}", name, price);
    }
    for (name, price) in &orca_prices {
        println!("{:<25} ${:>10.4}", name, price);
    }
    for (name, price) in &dlmm_prices {
        println!("{:<25} ${:>10.4}", name, price);
    }
    
    // Calculate spread across all pools
    let all_prices: Vec<f64> = raydium_prices.iter()
        .chain(orca_prices.iter())
        .chain(dlmm_prices.iter())
        .map(|(_, p)| *p)
        .filter(|p| *p > 50.0 && *p < 500.0) // Filter out stale pools
        .collect();
    
    if all_prices.len() >= 2 {
        let min_price = all_prices.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_price = all_prices.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let spread_bps = ((max_price - min_price) / min_price * 10000.0) as i32;
        println!("\nCross-DEX price spread: {} bps ({:.4} - {:.4})", spread_bps, min_price, max_price);
    }
    
    Ok(())
}

fn fetch_raydium_clmm_price(rpc: &RpcClient, pool_addr: &Pubkey, name: &str, decimals_a: u8, decimals_b: u8) -> Result<f64> {
    println!("Pool: {} ({})", name, pool_addr);
    
    let account = rpc.get_account(pool_addr)?;
    let data = &account.data;
    
    if data.len() < 300 {
        return Err(anyhow::anyhow!("Account data too small: {} bytes", data.len()));
    }
    
    // Parse liquidity (u128)
    let liquidity = u128::from_le_bytes(
        data[RAYDIUM_LIQUIDITY_OFFSET..RAYDIUM_LIQUIDITY_OFFSET+16]
            .try_into()?
    );
    
    // Parse sqrt_price_x64 (u128)
    let sqrt_price_x64 = u128::from_le_bytes(
        data[RAYDIUM_SQRT_PRICE_OFFSET..RAYDIUM_SQRT_PRICE_OFFSET+16]
            .try_into()?
    );
    
    // Parse current_tick (i32)
    let current_tick = i32::from_le_bytes([
        data[RAYDIUM_TICK_CURRENT_OFFSET],
        data[RAYDIUM_TICK_CURRENT_OFFSET+1],
        data[RAYDIUM_TICK_CURRENT_OFFSET+2],
        data[RAYDIUM_TICK_CURRENT_OFFSET+3],
    ]);
    
    // Calculate price from sqrt_price
    let sqrt_price_f64 = sqrt_price_x64 as f64;
    let q64_f64 = Q64 as f64;
    let price_raw = (sqrt_price_f64 / q64_f64).powi(2);
    
    // Adjust for decimals
    let decimal_adjustment = 10f64.powi(decimals_a as i32 - decimals_b as i32);
    let price = price_raw * decimal_adjustment;
    
    println!("  sqrt_price_x64: {}", sqrt_price_x64);
    println!("  liquidity:      {}", liquidity);
    println!("  current_tick:   {}", current_tick);
    println!("  Price:          ${:.4}", price);
    println!();
    
    Ok(price)
}

fn fetch_orca_whirlpool_price(rpc: &RpcClient, pool_addr: &Pubkey, name: &str, decimals_a: u8, decimals_b: u8) -> Result<f64> {
    println!("Pool: {} ({})", name, pool_addr);
    
    let account = rpc.get_account(pool_addr)?;
    let data = &account.data;
    
    if data.len() < 300 {
        return Err(anyhow::anyhow!("Account data too small: {} bytes", data.len()));
    }
    
    // Parse sqrt_price_x64 (u128) - at different offset than Raydium
    let sqrt_price_x64 = u128::from_le_bytes(
        data[ORCA_SQRT_PRICE_OFFSET..ORCA_SQRT_PRICE_OFFSET+16]
            .try_into()?
    );
    
    // Parse current_tick (i32)
    let current_tick = i32::from_le_bytes([
        data[ORCA_TICK_CURRENT_OFFSET],
        data[ORCA_TICK_CURRENT_OFFSET+1],
        data[ORCA_TICK_CURRENT_OFFSET+2],
        data[ORCA_TICK_CURRENT_OFFSET+3],
    ]);
    
    // Parse liquidity (u128)
    let liquidity = u128::from_le_bytes(
        data[ORCA_LIQUIDITY_OFFSET..ORCA_LIQUIDITY_OFFSET+16]
            .try_into()?
    );
    
    // Calculate price from sqrt_price
    let sqrt_price_f64 = sqrt_price_x64 as f64;
    let q64_f64 = Q64 as f64;
    let price_raw = (sqrt_price_f64 / q64_f64).powi(2);
    
    // Adjust for decimals
    let decimal_adjustment = 10f64.powi(decimals_a as i32 - decimals_b as i32);
    let price = price_raw * decimal_adjustment;
    
    println!("  sqrt_price_x64: {}", sqrt_price_x64);
    println!("  liquidity:      {}", liquidity);
    println!("  current_tick:   {}", current_tick);
    println!("  Price:          ${:.4}", price);
    println!();
    
    Ok(price)
}

/// Fetch price from Meteora DLMM pool
/// Price = (1 + bin_step/10000)^active_bin_id
fn fetch_dlmm_price(rpc: &RpcClient, pool_addr: &Pubkey, name: &str, decimals_a: u8, decimals_b: u8) -> Result<f64> {
    println!("Pool: {} ({})", name, pool_addr);
    
    let account = rpc.get_account(pool_addr)?;
    let data = &account.data;
    
    // DLMM LbPair accounts are 904 bytes
    if data.len() < 200 {
        return Err(anyhow::anyhow!("Account data too small: {} bytes (expected ~904)", data.len()));
    }
    
    // Parse active_bin_id (i32 at offset 72)
    let active_bin_id = i32::from_le_bytes([
        data[DLMM_ACTIVE_ID_OFFSET],
        data[DLMM_ACTIVE_ID_OFFSET+1],
        data[DLMM_ACTIVE_ID_OFFSET+2],
        data[DLMM_ACTIVE_ID_OFFSET+3],
    ]);
    
    // Parse bin_step (u16 at offset 76)
    let bin_step = u16::from_le_bytes([
        data[DLMM_BIN_STEP_OFFSET],
        data[DLMM_BIN_STEP_OFFSET+1],
    ]);
    
    // Calculate price: (1 + bin_step/10000)^active_bin_id
    // bin_step is in basis points (e.g., 1 = 0.01%, 100 = 1%)
    let base = 1.0 + (bin_step as f64) / 10000.0;
    let price_raw = base.powi(active_bin_id);
    
    // Adjust for decimals
    let decimal_adjustment = 10f64.powi(decimals_a as i32 - decimals_b as i32);
    let price = price_raw * decimal_adjustment;
    
    println!("  active_bin_id:  {}", active_bin_id);
    println!("  bin_step:       {} bps", bin_step);
    println!("  base:           {:.6}", base);
    println!("  Price:          ${:.4}", price);
    println!();
    
    Ok(price)
}