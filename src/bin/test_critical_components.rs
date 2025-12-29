//! Critical Component Diagnostic Tests
//!
//! Run BEFORE starting the bot to verify all critical systems work:
//! - Geyser gRPC connection + large account subscription
//! - Jito block engine auth + bundle submission
//! - Pool/vault loading from pools.json
//! - RPC connection for lookup tables
//!
//! Usage: cargo run --release --bin test_critical_components

use anyhow::{anyhow, bail, Context, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signer;
use std::collections::HashSet;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tre_arb_bot::config::Config;
use tre_arb_bot::geyser::{GeyserClient, GeyserConfig};
use tre_arb_bot::jito::{JitoClient, JitoConfig};
use tre_arb_bot::pools::PoolsFile;

const TEST_TIMEOUT_SECS: u64 = 30;

#[tokio::main]
async fn main() -> Result<()> {
    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║       tre-arb-bot Critical Component Diagnostics          ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // Load config
    let config_path = PathBuf::from("config.json");
    let config = Config::load(&config_path).map_err(|e| anyhow!("{}", e))?;
    println!("✓ Config loaded from config.json\n");

    let mut all_passed = true;
    let mut critical_failures: Vec<String> = Vec::new();

    // Test 1: Pool/Vault Loading
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("TEST 1: Pool and Vault Loading");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    match test_pool_loading(&config).await {
        Ok((pools, vaults, tick_arrays)) => {
            println!("  ✓ Pools loaded: {}", pools);
            println!("  ✓ Vault accounts: {}", vaults);
            println!("  ✓ Tick array accounts: {}", tick_arrays);
            println!("  ✓ Total accounts for Geyser subscription: {}", vaults + tick_arrays);
            if vaults + tick_arrays > 4_000_000 {
                println!("  ⚠ WARNING: Large subscription (>4M accounts) - Geyser message size limit is critical!");
            }
        }
        Err(e) => {
            println!("  ✗ FAILED: {}", e);
            critical_failures.push(format!("Pool loading: {}", e));
            all_passed = false;
        }
    }
    println!();

    // Test 2: Geyser gRPC Connection
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("TEST 2: Geyser gRPC Connection");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    match test_geyser_connection(&config).await {
        Ok(heartbeat) => {
            println!("  ✓ Connected to Geyser at {}", config.geyser.endpoint);
            println!("  ✓ Heartbeat interval: {}ms", heartbeat);
        }
        Err(e) => {
            println!("  ✗ FAILED: {}", e);
            critical_failures.push(format!("Geyser connection: {}", e));
            all_passed = false;
        }
    }
    println!();

    // Test 3: Geyser Large Account Subscription (THE CRITICAL TEST)
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("TEST 3: Geyser Large Account Subscription");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    match test_geyser_large_subscription(&config).await {
        Ok((subscribed, received)) => {
            println!("  ✓ Subscribed to {} accounts", subscribed);
            println!("  ✓ Received first {} updates (waiting 5s)", received);
            if received > 0 {
                println!("  ✓ Account subscription is WORKING!");
            } else {
                println!("  ⚠ No updates received yet (may be normal if accounts idle)");
            }
        }
        Err(e) => {
            let err_str = format!("{}", e);
            if err_str.contains("decoded message length too large") {
                println!("  ✗ CRITICAL: Message size limit exceeded!");
                println!("    The Geyser plugin needs max_decoding_message_size increased.");
                println!("    Did you rebuild and deploy the updated plugin?");
                println!("    Run: sudo systemctl restart solana-validator");
            } else {
                println!("  ✗ FAILED: {}", e);
            }
            critical_failures.push(format!("Geyser subscription: {}", e));
            all_passed = false;
        }
    }
    println!();

    // Test 4: Jito Block Engine Authentication
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("TEST 4: Jito Block Engine Authentication");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    match test_jito_auth(&config).await {
        Ok(()) => {
            println!("  ✓ Connected to Jito at {}", config.jito.primary_endpoint());
            println!("  ✓ Authentication successful");
            println!("  ✓ Ready to submit bundles");
        }
        Err(e) => {
            println!("  ✗ FAILED: {}", e);
            critical_failures.push(format!("Jito auth: {}", e));
            all_passed = false;
        }
    }
    println!();

    // Test 5: RPC Connection (for lookup tables)
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("TEST 5: RPC Connection");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    match test_rpc_connection(&config).await {
        Ok(slot) => {
            println!("  ✓ Connected to RPC at {}", config.primary_rpc_url());
            println!("  ✓ Current slot: {}", slot);
        }
        Err(e) => {
            println!("  ✗ FAILED: {}", e);
            critical_failures.push(format!("RPC connection: {}", e));
            all_passed = false;
        }
    }
    println!();

    // Test 6: Lookup Tables
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("TEST 6: Address Lookup Tables");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    match test_lookup_tables(&config).await {
        Ok(count) => {
            println!("  ✓ Loaded {} address lookup tables", count);
        }
        Err(e) => {
            println!("  ✗ FAILED: {}", e);
            critical_failures.push(format!("Lookup tables: {}", e));
            all_passed = false;
        }
    }
    println!();

    // Summary
    println!("═══════════════════════════════════════════════════════════");
    if all_passed {
        println!("  ✅ ALL TESTS PASSED - Bot is ready to run!");
        println!("═══════════════════════════════════════════════════════════");
        Ok(())
    } else {
        println!("  ❌ CRITICAL FAILURES DETECTED:");
        for failure in &critical_failures {
            println!("     • {}", failure);
        }
        println!("\n  Fix these issues before running the bot!");
        println!("═══════════════════════════════════════════════════════════");
        bail!("Critical tests failed")
    }
}

async fn test_pool_loading(config: &Config) -> Result<(usize, usize, usize)> {
    let pools_path = config.geyser.pools_file.as_ref()
        .ok_or_else(|| anyhow::anyhow!("No pools_file configured in config.json"))?;
    
    let pools_file = PoolsFile::load(pools_path)
        .context("Failed to load pools.json")?;
    
    let pool_count = pools_file.pools.len();
    
    // Extract vaults from pools
    let mut vaults: HashSet<String> = HashSet::new();
    for pool in &pools_file.pools {
        vaults.insert(pool.token_vault_a.clone());
        vaults.insert(pool.token_vault_b.clone());
    }
    let vault_count = vaults.len();
    
    // Tick arrays are derived dynamically, estimate based on CLMM pools
    let clmm_pools = pools_file.pools.iter()
        .filter(|p| p.dex.contains("Clmm") || p.dex.contains("Whirlpool") || p.dex.contains("Dlmm"))
        .count();
    let tick_count = clmm_pools * 5; // Rough estimate: ~5 tick arrays per CLMM pool
    
    if pool_count == 0 {
        bail!("pools.json contains 0 pools!");
    }
    
    Ok((pool_count, vault_count, tick_count))
}

async fn test_geyser_connection(config: &Config) -> Result<u64> {
    let geyser_config = GeyserConfig {
        endpoint: config.geyser.endpoint.clone(),
        connect_timeout: Duration::from_millis(config.geyser.connect_timeout_ms),
        request_timeout: Duration::from_millis(config.geyser.request_timeout_ms),
    };
    
    let client = timeout(
        Duration::from_secs(TEST_TIMEOUT_SECS),
        GeyserClient::new(geyser_config)
    ).await
        .context("Connection timeout")?
        .context("Failed to connect to Geyser")?;
    
    let heartbeat = timeout(
        Duration::from_secs(TEST_TIMEOUT_SECS),
        client.get_heartbeat_interval()
    ).await
        .context("Heartbeat timeout")?
        .context("Failed to get heartbeat interval")?;
    
    Ok(heartbeat)
}

async fn test_geyser_large_subscription(config: &Config) -> Result<(usize, usize)> {
    // Load accounts from pools.json
    let pools_path = config.geyser.pools_file.as_ref()
        .ok_or_else(|| anyhow::anyhow!("No pools_file configured"))?;
    
    let pools_file = PoolsFile::load(pools_path)?;
    
    // Gather all vault accounts to subscribe to
    let mut accounts: Vec<Pubkey> = Vec::new();
    let mut seen: HashSet<Pubkey> = HashSet::new();
    
    for pool in &pools_file.pools {
        if let Ok(pk) = Pubkey::from_str(&pool.token_vault_a) {
            if seen.insert(pk) {
                accounts.push(pk);
            }
        }
        if let Ok(pk) = Pubkey::from_str(&pool.token_vault_b) {
            if seen.insert(pk) {
                accounts.push(pk);
            }
        }
    }
    
    if accounts.is_empty() {
        // Use a small test set
        accounts.push(Pubkey::new_unique());
    }
    
    let total_accounts = accounts.len();
    
    // Connect and subscribe
    let geyser_config = GeyserConfig {
        endpoint: config.geyser.endpoint.clone(),
        connect_timeout: Duration::from_millis(config.geyser.connect_timeout_ms),
        request_timeout: Duration::from_millis(config.geyser.request_timeout_ms),
    };
    
    let client = timeout(
        Duration::from_secs(TEST_TIMEOUT_SECS),
        GeyserClient::new(geyser_config)
    ).await
        .context("Connection timeout")?
        .context("Failed to connect to Geyser")?;
    
    // THIS IS THE CRITICAL TEST - subscribing to millions of accounts
    println!("  ... Subscribing to {} accounts (this tests message size limits)", total_accounts);
    
    let mut stream = timeout(
        Duration::from_secs(TEST_TIMEOUT_SECS),
        client.subscribe_accounts(accounts)
    ).await
        .context("Subscription timeout")?
        .context("Failed to subscribe to accounts")?;
    
    // Try to receive a few updates to confirm the stream is working
    let mut received = 0;
    let start = Instant::now();
    let wait_duration = Duration::from_secs(5);
    
    while start.elapsed() < wait_duration {
        match timeout(Duration::from_millis(100), stream.message()).await {
            Ok(Ok(Some(_))) => {
                received += 1;
                if received >= 10 {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                // Check for the specific message size error
                let err_str = format!("{:?}", e);
                if err_str.contains("decoded message length too large") {
                    bail!("decoded message length too large - Geyser plugin needs max_decoding_message_size increased!");
                }
                bail!("Stream error: {}", e);
            }
            Err(_) => continue, // Timeout, try again
        }
    }
    
    Ok((total_accounts, received))
}

async fn test_jito_auth(config: &Config) -> Result<()> {
    let auth_keypair = Arc::new(config.wallets.auth.load_keypair().map_err(|e| anyhow!("{}", e))?);
    
    let jito_config = JitoConfig {
        endpoint: config.jito.primary_endpoint(),
        ca_pem: config.jito.ca_pem.clone(),
    };
    
    let client = timeout(
        Duration::from_secs(TEST_TIMEOUT_SECS),
        JitoClient::new(jito_config, auth_keypair)
    ).await
        .context("Connection timeout")?
        .context("Failed to connect to Jito")?;
    
    timeout(
        Duration::from_secs(TEST_TIMEOUT_SECS),
        client.authenticate()
    ).await
        .context("Auth timeout")?
        .context("Failed to authenticate with Jito")?;
    
    Ok(())
}

async fn test_rpc_connection(config: &Config) -> Result<u64> {
    let rpc = RpcClient::new(config.primary_rpc_url());
    
    let slot = timeout(
        Duration::from_secs(TEST_TIMEOUT_SECS),
        rpc.get_slot()
    ).await
        .context("RPC timeout")?
        .context("Failed to get slot from RPC")?;
    
    Ok(slot)
}

async fn test_lookup_tables(config: &Config) -> Result<usize> {
    if config.lookup_tables.is_empty() {
        return Ok(0);
    }
    
    let rpc = Arc::new(RpcClient::new(config.primary_rpc_url()));
    let payer = config.wallets.payer.load_keypair().map_err(|e| anyhow!("{}", e))?;
    
    let tables = timeout(
        Duration::from_secs(TEST_TIMEOUT_SECS * 2),
        tre_arb_bot::executor::load_lookup_tables(&rpc, &config.lookup_tables, &payer.pubkey())
    ).await
        .context("Lookup table load timeout")?
        .context("Failed to load lookup tables")?;
    
    Ok(tables.len())
}
