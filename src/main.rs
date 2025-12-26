//! MEV Bot - ShredStream + Geyser unified architecture
//!
//! Uses ShredStream for early TX detection and Geyser for ground truth confirmation.
//! Both sources work together via PredictiveStateManager for optimal MEV extraction.

use clap::Parser;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signer;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tre_arb_bot::config::Config;
use tre_arb_bot::executor::{load_lookup_tables, FlashLoanReserve};
use tre_arb_bot::jito::{JitoClient, JitoConfig as JitoClientConfig};
use tre_arb_bot::shredstream::{
    spawn_unified_pipeline, BundleBuilderConfig, UnifiedPipelineConfig,
};
use tre_arb_bot::tip_monitor::TipFloorMonitor;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "tre MEV bot - local validator Geyser backrunner"
)]
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

    run_mev_pipeline(config).await
}

/// Run the unified ShredStream + Geyser MEV pipeline
async fn run_mev_pipeline(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Starting tre MEV bot (ShredStream + Geyser unified pipeline)");
    println!("tre MEV bot started. Logs: /tmp/tre-arb.log. Press Ctrl+C to stop.");

    if !config.geyser.enabled {
        return Err("Geyser must be enabled. Set geyser.enabled=true in config".into());
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

    // Start tip floor monitor for competitive tipping
    let tip_monitor = TipFloorMonitor::new(config.jito.tip_floor_url.clone());
    let (_tip_handle, tip_floor_cache) = tip_monitor.spawn();
    tracing::info!("Tip floor monitor started (polling Jito tip floor API)");

    // Load Address Lookup Tables using local RPC
    let rpc_client = Arc::new(RpcClient::new(config.primary_rpc_url()));
    tracing::info!("Using RPC: {}", config.primary_rpc_url());
    let lookup_tables =
        load_lookup_tables(&rpc_client, &config.lookup_tables, &payer_pubkey).await?;
    tracing::info!("Loaded {} address lookup tables", lookup_tables.len());

    // Build unified pipeline config from MEV settings
    let mev_config = &config.shredstream.mev;
    let turbine_config = &config.shredstream.turbine;

    // Log the TX source mode
    if turbine_config.enabled {
        tracing::info!("=== Turbine UDP + Geyser Unified Pipeline ===");
        tracing::info!("TX Source: Direct Turbine UDP (ultra-low-latency ~0.5-2ms)");
        tracing::info!("Turbine bind address: {}", turbine_config.bind_addr);
        tracing::info!("Turbine receiver threads: {}", turbine_config.receiver_threads);
    } else {
        tracing::info!("=== ShredStream + Geyser Unified Pipeline ===");
        tracing::info!("TX Source: ShredStream proxy (fallback ~5-10ms)");
        tracing::info!("ShredStream endpoint: {}", config.shredstream.endpoint);
    }
    tracing::info!("Geyser endpoint: {}", config.geyser.endpoint);
    tracing::info!(
        "MEV config: backrun={} min_profit={} lamports",
        mev_config.backrun_enabled,
        mev_config.min_profit_lamports
    );

    let pipeline_config = UnifiedPipelineConfig {
        use_turbine_udp: turbine_config.enabled,
        turbine_config: turbine_config.clone(),
        shredstream_endpoint: config.shredstream.endpoint.clone(),
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
            tip_floor_multiplier: 2.0,
        },
        submission_timeout: Duration::from_secs(5),
        min_submission_interval: Duration::from_millis(200),
    };

    // Spawn unified pipeline - ShredStream + Geyser running together
    let (pipeline, stats) = spawn_unified_pipeline(
        pipeline_config,
        Some(tip_floor_cache),
        payer_keypair,
        flash_loan_reserves,
        lookup_tables,
        jito_client,
    )
    .await?;

    tracing::info!("MEV pipeline started (shredstream + geyser + predictive_state)");

    // Note: Pool state updates (DEX program accounts, vault balances) are now handled
    // by the unified pipeline's consolidated Geyser connection - no separate task needed.
    tracing::info!("Pool state updates consolidated into unified pipeline Geyser connection");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;

    // Log final stats
    let s = stats.as_ref();
    tracing::info!(
        "FINAL STATS: shred_entries={} shred_swaps={} geyser_txs={} pending_added={} pending_confirmed={} opps={} sent={} accepted={} profit={:.6} SOL",
        s.shredstream_entries_received.load(std::sync::atomic::Ordering::Relaxed),
        s.shredstream_swaps_detected.load(std::sync::atomic::Ordering::Relaxed),
        s.geyser_txs_received.load(std::sync::atomic::Ordering::Relaxed),
        s.pending_swaps_added.load(std::sync::atomic::Ordering::Relaxed),
        s.pending_swaps_confirmed.load(std::sync::atomic::Ordering::Relaxed),
        s.opportunities_found.load(std::sync::atomic::Ordering::Relaxed),
        s.bundles_submitted.load(std::sync::atomic::Ordering::Relaxed),
        s.bundles_accepted.load(std::sync::atomic::Ordering::Relaxed),
        s.total_profit_lamports.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9
    );

    // Log predictive state stats
    tracing::info!("PREDICTIVE STATE: {}", pipeline.predictive_state_stats());

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
