//! MEV Bot - Geyser-first architecture for local validator
//!
//! Uses local Geyser plugin for ultra-low-latency transaction interception and backrunning.
//! All data comes from your local validator - no external API dependencies.

use clap::Parser;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signer;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tre_arb_bot::amm_math::PoolStateCache;
use tre_arb_bot::config::Config;
use tre_arb_bot::executor::{load_lookup_tables, FlashLoanReserve};
use tre_arb_bot::jito::{JitoClient, JitoConfig as JitoClientConfig};
use tre_arb_bot::geyser::GeyserConfig;
use tre_arb_bot::pool_discovery::{spawn_pool_state_updater, PoolDiscoveryService};
use tre_arb_bot::shredstream::{
    spawn_geyser_pipeline, BundleBuilderConfig, GeyserPipelineConfig,
};

#[derive(Parser, Debug)]
#[command(author, version, about = "tre MEV bot - local validator Geyser backrunner")]
struct Args {
    /// Path to the configuration file.
    #[arg(long, short = 'c', default_value = "config.json")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing to file
    let log_file = std::fs::File::create("/tmp/tre-arb.log")?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("tre_arb_bot=debug".parse().unwrap())
                .add_directive("info".parse().unwrap()),
        )
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false)
        .init();

    let args = Args::parse();
    let config = Config::load(&args.config)?;

    // Geyser mode - direct local validator access
    run_geyser_mode(config).await
}

/// Geyser mode: Direct connection to local validator's Geyser plugin
async fn run_geyser_mode(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Starting tre MEV bot (Geyser-first architecture for local validator)");
    println!(
        "tre MEV bot started (Geyser mode). Logs: /tmp/tre-arb.log. Press Ctrl+C to stop."
    );

    if !config.geyser.enabled {
        return Err(
            "Geyser must be enabled. Set geyser.enabled=true in config"
                .into(),
        );
    }

    // Load keypairs
    let auth_keypair = Arc::new(config.wallets.auth.load_keypair()?);
    let payer_keypair = Arc::new(config.wallets.payer.load_keypair()?);
    let payer_pubkey = payer_keypair.pubkey();

    tracing::info!("Payer: {}", payer_pubkey);

    // Initialize Jito client
    let jito_config = JitoClientConfig {
        endpoint: config.jito.primary_endpoint(),
        ca_pem: config.jito.ca_pem.clone(),
    };
    let jito_client = Arc::new(JitoClient::new(jito_config, auth_keypair.clone()).await?);
    tracing::info!(
        "Connected to Jito block engine: {}",
        config.jito.primary_endpoint()
    );

    // Build flash loan reserves
    let flash_loan_reserves = build_flash_loan_reserves(&config)?;
    tracing::info!("Loaded {} flash loan reserves", flash_loan_reserves.len());

    // Load Address Lookup Tables using local RPC
    let rpc_client = Arc::new(RpcClient::new(config.primary_rpc_url()));
    tracing::info!("Using RPC: {}", config.primary_rpc_url());
    let lookup_tables =
        load_lookup_tables(&rpc_client, &config.lookup_tables, &payer_pubkey).await?;
    tracing::info!("Loaded {} address lookup tables", lookup_tables.len());

    // Initialize pool state cache
    let pool_states = Arc::new(PoolStateCache::new());

    // Dynamic pool discovery - spawn in background to not block startup
    let pool_discovery = PoolDiscoveryService::new(Arc::clone(&rpc_client));
    let pool_registry = pool_discovery.registry();
    
    // Spawn background pool bootstrap (don't wait for it)
    let bootstrap_registry = Arc::clone(&pool_registry);
    let bootstrap_pool_states = Arc::clone(&pool_states);
    tokio::spawn(async move {
        tracing::info!("Background: Bootstrapping pool discovery from on-chain data...");
        if let Err(e) = pool_discovery.bootstrap().await {
            tracing::warn!("Pool discovery bootstrap failed: {}. Will discover pools from transactions.", e);
        } else {
            let count = bootstrap_registry.len();
            tracing::info!("Background: Discovered {} pools from on-chain data", count);
            // Sync discovered pools to pool state cache
            bootstrap_registry.sync_to_pool_state_cache(&bootstrap_pool_states, 0);
        }
    });
    tracing::info!("Pool discovery spawned in background (proceeding with Geyser pipeline)");

    // Spawn real-time pool state updater via Geyser
    let geyser_update_config = GeyserConfig {
        endpoint: config.geyser.endpoint.clone(),
        connect_timeout: Duration::from_millis(config.geyser.connect_timeout_ms),
        request_timeout: Duration::from_millis(config.geyser.request_timeout_ms),
    };
    spawn_pool_state_updater(
        geyser_update_config,
        Arc::clone(&pool_registry),
        Arc::clone(&pool_states),
    ).await?;
    tracing::info!("Pool state updater started (real-time Geyser updates)");

    // Build Geyser pipeline config from MEV settings
    let mev_config = &config.shredstream.mev;
    let geyser_pipeline_config = GeyserPipelineConfig {
        geyser_endpoint: config.geyser.endpoint.clone(),
        connect_timeout: Duration::from_millis(config.geyser.connect_timeout_ms),
        request_timeout: Duration::from_millis(config.geyser.request_timeout_ms),
        skip_votes: config.geyser.skip_votes,
        bundle_config: BundleBuilderConfig {
            compute_units: mev_config.compute_units,
            compute_unit_price: mev_config.compute_unit_price,
            base_tip_lamports: 10_000,
            tip_profit_share_bps: mev_config.tip_profit_share_bps,
            max_tip_profit_bps: 8000,
            slippage_bps: 100,
        },
        submission_timeout: Duration::from_secs(5),
        min_submission_interval: Duration::from_millis(200),
    };

    tracing::info!("Geyser endpoint: {}", config.geyser.endpoint);
    tracing::info!(
        "MEV config: backrun={} sandwich={} min_profit={} lamports",
        mev_config.backrun_enabled,
        mev_config.sandwich_enabled,
        mev_config.min_profit_lamports
    );

    // Spawn the Geyser MEV pipeline
    let (stats, _blockhash_arc) = spawn_geyser_pipeline(
        geyser_pipeline_config,
        pool_states,
        Some(pool_registry),
        payer_keypair,
        flash_loan_reserves,
        lookup_tables,
        jito_client,
    )
    .await?;

    tracing::info!("Geyser MEV pipeline started, waiting for transaction data...");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;

    // Log final stats
    let s = stats.as_ref();
    tracing::info!(
        "FINAL STATS: rx={} votes_skip={} swaps={} opps={} sent={} accepted={} profit={:.6} SOL",
        s.transactions_received
            .load(std::sync::atomic::Ordering::Relaxed),
        s.vote_txs_skipped
            .load(std::sync::atomic::Ordering::Relaxed),
        s.swaps_detected.load(std::sync::atomic::Ordering::Relaxed),
        s.opportunities_found
            .load(std::sync::atomic::Ordering::Relaxed),
        s.bundles_submitted
            .load(std::sync::atomic::Ordering::Relaxed),
        s.bundles_accepted
            .load(std::sync::atomic::Ordering::Relaxed),
        s.total_profit_lamports
            .load(std::sync::atomic::Ordering::Relaxed) as f64
            / 1e9
    );

    Ok(())
}

fn build_flash_loan_reserves(
    config: &Config,
) -> Result<HashMap<Pubkey, FlashLoanReserve>, Box<dyn std::error::Error>> {
    let mut reserves = HashMap::new();
    for loan in config.solend_flash_loans.values() {
        let mint = Pubkey::from_str(&loan.reserve_liquidity_mint)?;
        reserves.insert(
            mint,
            FlashLoanReserve {
                program_id: Pubkey::from_str(&loan.program_id)?,
                lending_market: Pubkey::from_str(&loan.lending_market)?,
                reserve: Pubkey::from_str(&loan.reserve)?,
                reserve_liquidity_mint: mint,
                borrow_liquidity_supply: Pubkey::from_str(&loan.borrow.reserve_liquidity_supply)?,
                repay_liquidity_supply: Pubkey::from_str(&loan.repay.reserve_liquidity_supply)?,
                repay_fee_receiver: Pubkey::from_str(&loan.repay.reserve_liquidity_fee_receiver)?,
                host_fee_receiver: Pubkey::from_str(&loan.repay.host_fee_receiver)?,
                max_borrow: loan.max_borrow,
                min_borrow: loan.min_borrow,
            },
        );
    }
    Ok(reserves)
}
