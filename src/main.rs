//! tre Arb Bot - Turbine + Geyser unified architecture
//!
//! Uses Turbine UDP for fast shred reception and Geyser for authoritative state.
//! Both sources work together via PoolStateManager for cross-pool arbitrage.

// Use jemalloc for better allocation performance in latency-sensitive code
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use clap::Parser;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signer;
// Use FxHashMap for faster lookups
use rustc_hash::FxHashMap as HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tre_arb_bot::config::Config;
use tre_arb_bot::executor::{load_lookup_tables, FlashLoanReserve};
use tre_arb_bot::jito::{JitoClient, JitoConfig as JitoClientConfig};
use tre_arb_bot::pools::PoolsFile;
use tre_arb_bot::pool_state::PoolStateManager;
use tre_arb_bot::shredstream::{
    spawn_unified_pipeline, BundleBuilderConfig, UnifiedPipelineConfig,
};
use tre_arb_bot::tip_monitor::TipFloorMonitor;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "tre arb bot - cross-pool arbitrage with Turbine + Geyser"
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

    run_arb_pipeline(config).await
}

/// Run the unified Turbine + Geyser arb pipeline
async fn run_arb_pipeline(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Starting tre arb bot (Turbine + Geyser unified pipeline)");
    println!("tre arb bot started. Logs: /tmp/tre-arb.log. Press Ctrl+C to stop.");

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

    // Build unified pipeline config from arb settings
    let arb_config = &config.shredstream.arb;
    let turbine_config = &config.shredstream.turbine;

    // Log the TX source mode
    if turbine_config.enabled {
        tracing::info!("=== Turbine UDP + Geyser Unified Pipeline ===");
        tracing::info!("TX Source: Direct Turbine UDP (only faster if co-located with high-stake validator)");
        tracing::info!("Turbine bind address: {}", turbine_config.bind_addr);
        tracing::info!("Turbine receiver threads: {}", turbine_config.receiver_threads);
    } else {
        tracing::info!("=== Jito ShredStream + Geyser Unified Pipeline ===");
        tracing::info!("TX Source: Jito ShredStream proxy (stake-weighted, layer-1 access)");
        tracing::info!("ShredStream endpoint: {}", config.shredstream.endpoint);
    }
    tracing::info!("Geyser endpoint: {}", config.geyser.endpoint);
    tracing::info!(
        "Arb config: enabled={} min_profit={} lamports",
        arb_config.arb_enabled,
        arb_config.min_profit_lamports
    );

    let pipeline_config = UnifiedPipelineConfig {
        use_turbine_udp: turbine_config.enabled,
        turbine_config: turbine_config.clone(),
        shredstream_endpoint: config.shredstream.endpoint.clone(),
        rpc_url: config.primary_rpc_url(),
        geyser_endpoint: config.geyser.endpoint.clone(),
        connect_timeout: Duration::from_millis(config.geyser.connect_timeout_ms),
        request_timeout: Duration::from_millis(config.geyser.request_timeout_ms),
        bundle_config: BundleBuilderConfig {
            compute_units: arb_config.compute_units,
            compute_unit_price: arb_config.compute_unit_price,
            base_tip_lamports: arb_config.base_tip_lamports,
            tip_profit_share_bps: arb_config.tip_profit_share_bps,
            max_tip_profit_bps: arb_config.max_tip_profit_bps,
            slippage_bps: arb_config.slippage_bps,
            tip_floor_multiplier: arb_config.tip_floor_multiplier,
        },
        submission_timeout: Duration::from_secs(arb_config.submission_timeout_secs),
        min_submission_interval: Duration::from_millis(arb_config.min_submission_interval_ms),
    };

    // Load static pools file if configured (enables zero-RPC runtime)
    let existing_psm = if let Some(ref pools_path) = config.geyser.pools_file {
        tracing::info!("Loading static pools from: {}", pools_path);
        let pools_file = PoolsFile::load(pools_path)?;
        
        // Create and populate PSM with pre-loaded pools
        let psm = Arc::new(PoolStateManager::new());
        let registered = pools_file.register_with_psm(&psm)?;
        
        tracing::info!(
            "Pre-loaded {} pools, {} vaults ready for Geyser subscription",
            registered,
            psm.all_vaults().len()
        );
        
        Some(psm)
    } else {
        tracing::info!("No pools_file configured - pools will be discovered dynamically via Geyser");
        tracing::warn!("NOTE: Vault subscriptions will be empty until pools are discovered");
        None
    };

    // Spawn unified pipeline - ShredStream + Geyser running together
    let (pipeline, stats) = spawn_unified_pipeline(
        pipeline_config,
        Some(tip_floor_cache),
        payer_keypair,
        flash_loan_reserves,
        lookup_tables,
        jito_client,
        existing_psm,
    )
    .await?;

    tracing::info!("Arbitrage pipeline started (turbine + geyser + pool_state)");

    // Pool state updates (DEX program accounts, vault balances) are handled
    // by the unified pipeline's Geyser connection.
    tracing::info!("Pool state updates handled by unified pipeline Geyser connection");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;

    // Log final stats - show Turbine stats when using Turbine mode
    let s = stats.as_ref();
    let turbine_pkts = s.turbine_packets_received.load(std::sync::atomic::Ordering::Relaxed);
    let turbine_entries = s.turbine_entries_assembled.load(std::sync::atomic::Ordering::Relaxed);
    let shred_entries = s.shredstream_entries_received.load(std::sync::atomic::Ordering::Relaxed);
    
    // Show whichever source is active
    let (source_name, entries_count) = if turbine_pkts > 0 || turbine_entries > 0 {
        ("turbine", turbine_entries)
    } else {
        ("shredstream", shred_entries)
    };
    
    tracing::info!(
        "FINAL STATS: source={} entries={} swaps={} geyser_accts={} opps={} sent={} accepted={} profit={:.6} SOL",
        source_name,
        entries_count,
        s.shredstream_swaps_detected.load(std::sync::atomic::Ordering::Relaxed),
        s.geyser_account_updates.load(std::sync::atomic::Ordering::Relaxed),
        s.opportunities_found.load(std::sync::atomic::Ordering::Relaxed),
        s.bundles_submitted.load(std::sync::atomic::Ordering::Relaxed),
        s.bundles_accepted.load(std::sync::atomic::Ordering::Relaxed),
        s.total_profit_lamports.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9
    );
    
    // Show detailed Turbine stats if active
    if turbine_pkts > 0 {
        tracing::info!(
            "TURBINE: pkts={} data_shreds={} coding_shreds={} fec_complete={} entries={}",
            turbine_pkts,
            s.turbine_data_shreds.load(std::sync::atomic::Ordering::Relaxed),
            s.turbine_coding_shreds.load(std::sync::atomic::Ordering::Relaxed),
            s.turbine_fec_completed.load(std::sync::atomic::Ordering::Relaxed),
            turbine_entries,
        );
    }

    // Log pool state stats
    tracing::info!("POOL STATE: {}", pipeline.pool_state_stats());

    // Log evaluator stats (critical for debugging why no opps are found)
    tracing::info!("EVALUATOR: {}", pipeline.evaluator_stats().await);

    // Log pair index stats (critical for cross-pool arb routing)
    tracing::info!("PAIR INDEX: {}", pipeline.pair_index_stats());

    Ok(())
}

fn build_flash_loan_reserves(
    config: &Config,
) -> Result<HashMap<Pubkey, FlashLoanReserve>, Box<dyn std::error::Error>> {
    let mut reserves = HashMap::default();
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
