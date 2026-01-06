//! Biggie - Simplified Solana Arbitrage Bot
//!
//! Two tasks:
//! 1. Main loop: Helius WS → vault updates → check arb → submit
//! 2. Blockhash refresh: poll RPC every 400ms

use clap::Parser;
use solana_sdk::hash::Hash;
use solana_sdk::signer::Signer;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

mod arb;
mod bootstrap;
mod config;
mod executor;
mod helius;
mod math;
mod pools;

use arb::ArbDetector;
use config::Config;
use executor::{JitoClient, TxBuilder};
use helius::{parse_token_balance, Helius};
use pools::PoolRegistry;

#[derive(Parser)]
#[command(name = "biggie", about = "Simplified Solana arb bot")]
struct Args {
    /// Config file path
    #[arg(short, long, default_value = "config.json")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("biggie=debug,info")
        .init();

    let args = Args::parse();
    let config = Config::load(&args.config)?;

    tracing::info!("Biggie starting...");
    tracing::info!("Helius RPC: {}", config.helius_rpc);
    tracing::info!("Jito endpoint: {}", config.jito_endpoint);

    // Load keypair
    let payer = Arc::new(config.payer()?);
    tracing::info!("Payer: {}", payer.pubkey());

    // Load pools
    let mut registry = PoolRegistry::load(&config.pools_file)?;
    tracing::info!("Loaded {} pools", registry.pools.len());

    // Bootstrap CLMM pools (fetch tick arrays, amm_config, etc.)
    tracing::info!("Bootstrapping CLMM pools...");
    let rpc = solana_client::rpc_client::RpcClient::new(config.helius_rpc.clone());
    bootstrap::bootstrap_clmm_pools(&rpc, &mut registry)?;

    // Initialize clients
    let mut helius = Helius::new(config.helius_rpc.clone(), config.helius_ws.clone());
    let jito = JitoClient::new(config.jito_endpoint.clone());
    let tx_builder = TxBuilder::new(&config, payer.clone());
    let arb_detector = ArbDetector::new(config.min_profit_lamports, config.slippage_bps);

    // Connect to Helius WebSocket
    helius.connect().await?;

    // Subscribe to all vault accounts
    tracing::info!("Subscribing to {} vault accounts...", registry.all_vaults.len());
    helius.subscribe_accounts_tracked(&registry.all_vaults).await?;

    // Subscribe to CLMM pool state accounts (for tick tracking)
    if !registry.clmm_pools.is_empty() {
        tracing::info!("Subscribing to {} CLMM pool state accounts...", registry.clmm_pools.len());
        helius.subscribe_accounts_tracked(&registry.clmm_pools).await?;
    }

    // Shared blockhash cache
    let blockhash_cache = helius.blockhash_cache();

    // Spawn blockhash refresh task
    let blockhash_rpc = config.helius_rpc.clone();
    let blockhash_cache_clone = blockhash_cache.clone();
    tokio::spawn(async move {
        blockhash_loop(blockhash_rpc, blockhash_cache_clone).await;
    });

    // Initial blockhash fetch
    let initial_blockhash = helius.refresh_blockhash().await?;
    tracing::info!("Initial blockhash: {}", initial_blockhash);

    // Stats
    let mut updates_received = 0u64;
    let mut arbs_found = 0u64;
    let mut arbs_submitted = 0u64;

    tracing::info!("=== Listening for account updates ===");

    // Main loop: process account updates (vaults + CLMM pool states)
    loop {
        match helius.recv_update().await {
            Ok(update) => {
                updates_received += 1;

                // Check if this is a CLMM pool state update (for tick tracking)
                if registry.is_clmm_pool(&update.pubkey) {
                    if let Some(pair) = registry.update_clmm_tick(&update.pubkey, &update.data) {
                        tracing::trace!(
                            "CLMM tick update: pool={} slot={}",
                            update.pubkey,
                            update.slot
                        );
                        // Check for arb opportunity after tick update
                        if let Some(opp) = arb_detector.check_pair(&registry, &pair) {
                            arbs_found += 1;
                            tracing::info!(
                                "ARB FOUND (tick): buy={} sell={} profit={:.6} SOL ({} bps)",
                                opp.buy_pool,
                                opp.sell_pool,
                                opp.profit_sol(),
                                opp.spread_bps()
                            );
                            // TODO: Submit arb transaction
                        }
                    }
                    continue;
                }

                // Parse token balance (vault update)
                let balance = match parse_token_balance(&update.data) {
                    Some(b) => b,
                    None => continue,
                };

                // Update pool state
                let (pool_addr, pair) = match registry.update_vault(&update.pubkey, balance) {
                    Some(x) => x,
                    None => continue,
                };

                tracing::debug!(
                    "Vault update: {} balance={} pool={} slot={}",
                    update.pubkey,
                    balance,
                    pool_addr,
                    update.slot
                );

                // Check for arb opportunity
                if let Some(opp) = arb_detector.check_pair(&registry, &pair) {
                    arbs_found += 1;
                    tracing::info!(
                        "ARB FOUND: buy={} sell={} profit={:.6} SOL ({} bps)",
                        opp.buy_pool,
                        opp.sell_pool,
                        opp.profit_sol(),
                        opp.spread_bps()
                    );

                    // Get current blockhash
                    let blockhash = blockhash_cache.read().await.0;
                    if blockhash == Hash::default() {
                        tracing::warn!("No blockhash available, skipping");
                        continue;
                    }

                    // Build and submit transaction
                    match tx_builder.build_arb_tx(&opp, &registry, blockhash) {
                        Ok(tx) => {
                            match jito.send_bundle(vec![tx]).await {
                                Ok(bundle_id) => {
                                    arbs_submitted += 1;
                                    tracing::info!(
                                        "Bundle submitted: {} (found={} submitted={})",
                                        bundle_id,
                                        arbs_found,
                                        arbs_submitted
                                    );
                                }
                                Err(e) => {
                                    tracing::error!("Bundle submission failed: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("TX build failed: {}", e);
                        }
                    }
                }

                // Periodic stats
                if updates_received % 1000 == 0 {
                    tracing::info!(
                        "Stats: updates={} arbs_found={} arbs_submitted={}",
                        updates_received,
                        arbs_found,
                        arbs_submitted
                    );
                }
            }
            Err(e) => {
                tracing::error!("WebSocket error: {}", e);
                // Attempt reconnection
                tracing::info!("Attempting reconnection in 5s...");
                tokio::time::sleep(Duration::from_secs(5)).await;

                if let Err(e) = helius.connect().await {
                    tracing::error!("Reconnection failed: {}", e);
                    continue;
                }

                if let Err(e) = helius.subscribe_accounts_tracked(&registry.all_vaults).await {
                    tracing::error!("Resubscription failed: {}", e);
                }
            }
        }
    }
}

/// Background task to refresh blockhash
async fn blockhash_loop(rpc_url: String, cache: Arc<RwLock<(Hash, u64)>>) {
    let client = reqwest::Client::new();

    loop {
        tokio::time::sleep(Duration::from_millis(400)).await;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
            "params": [{"commitment": "confirmed"}]
        });

        match client.post(&rpc_url).json(&request).send().await {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(hash_str) = json
                        .get("result")
                        .and_then(|r| r.get("value"))
                        .and_then(|v| v.get("blockhash"))
                        .and_then(|b| b.as_str())
                    {
                        if let Ok(hash) = hash_str.parse::<Hash>() {
                            let mut guard = cache.write().await;
                            *guard = (hash, 0);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Blockhash refresh failed: {}", e);
            }
        }
    }
}
