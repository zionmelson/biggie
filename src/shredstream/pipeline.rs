//! Shredstream MEV Pipeline
//!
//! Main processing loop: shredstream → parse → evaluate → build → submit
//! This is the legacy shredstream-proxy based pipeline. For local validator
//! deployments, prefer the Geyser-based pipeline in geyser_pipeline.rs.

use super::backrun::{BackrunBundleBuilder, BundleBuilderConfig, SandwichBundle};
use super::evaluator::{MevEvaluator, MevOpportunity, PoolInfo, SandwichOpportunity};
use super::parser::parse_transaction;
use super::types::ShredstreamConfig;
use crate::amm_math::PoolStateCache;
use crate::executor::FlashLoanReserve;
use crate::pool_discovery::DynamicPoolRegistry;
use crate::jito::{BundleStatus, JitoClient};
use crate::tip_monitor::TipFloorCache;
use anyhow::{Context, Result};
use bincode::Options;
use solana_entry::entry::Entry as SolanaEntry;
use solana_message::AddressLookupTableAccount;
use solana_sdk::hash::Hash;
use solana_sdk::signature::Keypair;
use solana_sdk::transaction::VersionedTransaction;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tonic::transport::Channel;

// Import generated protobuf types
use crate::jito::shredstream::{
    shredstream_proxy_client::ShredstreamProxyClient, SubscribeEntriesRequest,
};

/// Blockhash update message (local definition to avoid external deps)
#[derive(Clone, Debug)]
pub struct BlockhashUpdate {
    pub hash: Hash,
    pub slot: u64,
}

/// Pipeline statistics
#[derive(Debug, Default)]
pub struct PipelineStats {
    pub entries_received: AtomicU64,
    pub transactions_parsed: AtomicU64,
    pub swaps_detected: AtomicU64,
    pub opportunities_found: AtomicU64,
    pub bundles_built: AtomicU64,
    pub bundles_submitted: AtomicU64,
    pub bundles_accepted: AtomicU64,
    pub bundles_rejected: AtomicU64,
    pub total_profit_lamports: AtomicU64,
    pub last_slot: AtomicU64,
}

/// Pipeline configuration
#[derive(Clone, Debug)]
pub struct PipelineConfig {
    /// Shredstream gRPC endpoint
    pub shredstream_endpoint: String,
    /// Bundle builder config
    pub bundle_config: BundleBuilderConfig,
    /// Maximum opportunities to queue
    pub max_opportunity_queue: usize,
    /// Bundle submission timeout
    pub submission_timeout: Duration,
    /// Minimum time between submissions (rate limiting)
    pub min_submission_interval: Duration,
    /// Whether sandwich attacks are enabled
    pub sandwich_enabled: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            shredstream_endpoint: "http://127.0.0.1:10000".to_string(),
            bundle_config: BundleBuilderConfig::default(),
            max_opportunity_queue: 100,
            submission_timeout: Duration::from_secs(5),
            min_submission_interval: Duration::from_millis(200),
            sandwich_enabled: false,
        }
    }
}

/// The main MEV pipeline
pub struct MevPipeline {
    config: PipelineConfig,
    evaluator: MevEvaluator,
    bundle_builder: BackrunBundleBuilder,
    jito_client: Arc<JitoClient>,
    blockhash: Arc<RwLock<(Hash, u64)>>, // (hash, slot)
    stats: Arc<PipelineStats>,
}

impl MevPipeline {
    pub fn new(
        config: PipelineConfig,
        pool_states: Arc<PoolStateCache>,
        pool_registry: Option<Arc<DynamicPoolRegistry>>,
        payer: Arc<Keypair>,
        flash_loan_reserves: HashMap<solana_sdk::pubkey::Pubkey, FlashLoanReserve>,
        lookup_tables: Vec<AddressLookupTableAccount>,
        jito_client: Arc<JitoClient>,
        tip_floor_cache: Option<Arc<TipFloorCache>>,
    ) -> Self {
        let evaluator = MevEvaluator::new(pool_states, pool_registry, config.sandwich_enabled);
        let mut bundle_builder = BackrunBundleBuilder::new(
            config.bundle_config.clone(),
            payer,
            flash_loan_reserves,
            lookup_tables,
        );
        
        // Set tip floor cache if available
        if let Some(cache) = tip_floor_cache {
            bundle_builder.set_tip_floor_cache(cache);
            tracing::info!("Bundle builder configured with dynamic tip floor monitoring");
        }

        Self {
            config,
            evaluator,
            bundle_builder,
            jito_client,
            blockhash: Arc::new(RwLock::new((Hash::default(), 0))),
            stats: Arc::new(PipelineStats::default()),
        }
    }

    /// Get pipeline statistics
    pub fn stats(&self) -> Arc<PipelineStats> {
        Arc::clone(&self.stats)
    }

    /// Update the blockhash used for transaction building
    pub async fn update_blockhash(&self, hash: Hash, slot: u64) {
        let mut guard = self.blockhash.write().await;
        *guard = (hash, slot);
    }

    /// Register a pool for arbitrage routing
    pub fn register_pool(&mut self, info: PoolInfo) {
        self.evaluator.register_pool(info);
    }

    /// Run the pipeline (main loop)
    pub async fn run(&mut self) -> Result<()> {
        tracing::info!(
            "Starting MEV pipeline, connecting to {}",
            self.config.shredstream_endpoint
        );

        // Connect to shredstream proxy with timeout to prevent blocking
        let channel = Channel::from_shared(self.config.shredstream_endpoint.clone())
            .context("Invalid shredstream endpoint")?
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .connect()
            .await
            .context("Failed to connect to shredstream proxy")?;

        let mut client = ShredstreamProxyClient::new(channel);

        // Subscribe to entries
        let request = tonic::Request::new(SubscribeEntriesRequest {});
        let mut stream = client
            .subscribe_entries(request)
            .await
            .context("Failed to subscribe to shredstream entries")?
            .into_inner();

        tracing::info!("Shredstream subscription established");

        // Bincode options matching Solana's serialization
        let bincode_opts = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .allow_trailing_bytes();

        let mut last_submission = Instant::now() - self.config.min_submission_interval;
        let mut last_registry_sync = Instant::now();
        let mut last_eval_log = Instant::now();
        let registry_sync_interval = Duration::from_secs(2); // Sync pools from registry frequently
        let eval_log_interval = Duration::from_secs(10);

        // Initial sync from registry
        self.evaluator.sync_from_registry();
        tracing::info!(
            "Initial pool sync: {} pools, {} routes",
            self.evaluator.known_pool_count(),
            self.evaluator.route_count()
        );

        // Main processing loop
        while let Some(entry_msg) = stream.message().await? {
            // Periodic sync from registry (picks up newly discovered pools)
            if last_registry_sync.elapsed() > registry_sync_interval {
                self.evaluator.sync_from_registry();
                last_registry_sync = Instant::now();
            }

            // Periodic evaluator stats logging
            if last_eval_log.elapsed() > eval_log_interval {
                tracing::debug!(
                    "EVALUATOR: pools={} routes={} | {}",
                    self.evaluator.known_pool_count(),
                    self.evaluator.route_count(),
                    self.evaluator.stats_summary()
                );
                last_eval_log = Instant::now();
            }

            let slot = entry_msg.slot;
            self.stats.last_slot.store(slot, Ordering::Relaxed);
            self.stats.entries_received.fetch_add(1, Ordering::Relaxed);

            // Deserialize entries
            let entries: Vec<SolanaEntry> = match bincode_opts.deserialize(&entry_msg.entries) {
                Ok(e) => e,
                Err(e) => {
                    tracing::trace!("Failed to deserialize entries: {}", e);
                    continue;
                }
            };

            let received_at = Instant::now();

            // Process each entry's transactions
            for entry in entries {
                for transaction in entry.transactions {
                    self.stats.transactions_parsed.fetch_add(1, Ordering::Relaxed);

                    let versioned_tx: VersionedTransaction = transaction.into();

                    // Parse for swaps
                    if let Some(tx_swaps) = parse_transaction(&versioned_tx, slot, received_at, None)
                    {
                        self.stats
                            .swaps_detected
                            .fetch_add(tx_swaps.swaps.len() as u64, Ordering::Relaxed);

                        // Evaluate for cross-pool backrun opportunities
                        let opportunities = self.evaluator.evaluate(&tx_swaps);

                        for opp in opportunities {
                            self.stats.opportunities_found.fetch_add(1, Ordering::Relaxed);

                            // Rate limit submissions
                            let elapsed = last_submission.elapsed();
                            if elapsed < self.config.min_submission_interval {
                                tokio::time::sleep(self.config.min_submission_interval - elapsed)
                                    .await;
                            }

                            // Build and submit bundle
                            match self.process_opportunity(&opp).await {
                                Ok(status) => {
                                    last_submission = Instant::now();
                                    self.handle_bundle_status(&opp.id, opp.expected_profit_lamports, status);
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        opp_id = %opp.id,
                                        error = %e,
                                        "Failed to process opportunity"
                                    );
                                }
                            }
                        }

                        // Evaluate for sandwich opportunities (if enabled)
                        let sandwich_opps = self.evaluator.evaluate_sandwich(&tx_swaps);

                        for opp in sandwich_opps {
                            self.stats.opportunities_found.fetch_add(1, Ordering::Relaxed);

                            // Rate limit submissions
                            let elapsed = last_submission.elapsed();
                            if elapsed < self.config.min_submission_interval {
                                tokio::time::sleep(self.config.min_submission_interval - elapsed)
                                    .await;
                            }

                            // Build and submit sandwich bundle
                            match self.process_sandwich_opportunity(&opp).await {
                                Ok(status) => {
                                    last_submission = Instant::now();
                                    self.handle_bundle_status(&opp.id, opp.expected_profit_lamports, status);
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        opp_id = %opp.id,
                                        error = %e,
                                        "Failed to process sandwich opportunity"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        tracing::warn!("Shredstream stream ended");
        Ok(())
    }

    /// Handle bundle status for logging and stats
    fn handle_bundle_status(&self, opp_id: &str, profit: u64, status: BundleStatus) {
        match status {
            BundleStatus::Submitted { .. } => {
                tracing::debug!(opp_id = %opp_id, "Bundle submitted, awaiting result");
            }
            BundleStatus::Accepted { .. }
            | BundleStatus::Finalized { .. }
            | BundleStatus::Processed { .. } => {
                self.stats.bundles_accepted.fetch_add(1, Ordering::Relaxed);
                self.stats.total_profit_lamports.fetch_add(profit, Ordering::Relaxed);
                tracing::info!(
                    opp_id = %opp_id,
                    profit = profit,
                    "Bundle accepted!"
                );
            }
            BundleStatus::Rejected { reason, .. } => {
                self.stats.bundles_rejected.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    opp_id = %opp_id,
                    reason = %reason,
                    "Bundle rejected"
                );
            }
            BundleStatus::Dropped { .. } => {
                self.stats.bundles_rejected.fetch_add(1, Ordering::Relaxed);
            }
            BundleStatus::Unknown { .. } => {}
        }
    }

    /// Process a single opportunity: build bundle and submit
    async fn process_opportunity(&self, opp: &MevOpportunity) -> Result<BundleStatus> {
        // Get current blockhash
        let (blockhash, _slot) = *self.blockhash.read().await;
        if blockhash == Hash::default() {
            anyhow::bail!("No blockhash available");
        }

        // Build bundle
        let bundle = self.bundle_builder.build(opp, blockhash)?;
        self.stats.bundles_built.fetch_add(1, Ordering::Relaxed);

        tracing::info!(
            opp_id = %opp.id,
            bundle_id = %bundle.id,
            profit = opp.expected_profit_lamports,
            tip = bundle.tip_lamports,
            "Submitting backrun bundle"
        );

        // Submit to Jito
        self.stats.bundles_submitted.fetch_add(1, Ordering::Relaxed);

        // Submit bundle to Jito
        let bundle_id = self
            .jito_client
            .send_versioned_bundle(bundle.transactions())
            .await?;

        tracing::debug!(bundle_id = %bundle_id, "Bundle submitted to Jito");

        // Return submitted status. Actual confirmation comes via bundle results stream.
        Ok(BundleStatus::Submitted { bundle_id })
    }

    /// Process a sandwich opportunity: build bundle and submit
    async fn process_sandwich_opportunity(&self, opp: &SandwichOpportunity) -> Result<BundleStatus> {
        // Get current blockhash
        let (blockhash, _slot) = *self.blockhash.read().await;
        if blockhash == Hash::default() {
            anyhow::bail!("No blockhash available");
        }

        // Build sandwich bundle
        let bundle = self.bundle_builder.build_sandwich(opp, blockhash)?;
        self.stats.bundles_built.fetch_add(1, Ordering::Relaxed);

        tracing::info!(
            opp_id = %opp.id,
            bundle_id = %bundle.id,
            profit = opp.expected_profit_lamports,
            tip = bundle.tip_lamports,
            frontrun = opp.frontrun_amount,
            "Submitting sandwich bundle"
        );

        // Submit to Jito
        self.stats.bundles_submitted.fetch_add(1, Ordering::Relaxed);

        // Submit bundle to Jito
        let bundle_id = self
            .jito_client
            .send_versioned_bundle(bundle.transactions())
            .await?;

        tracing::debug!(bundle_id = %bundle_id, "Sandwich bundle submitted to Jito");

        Ok(BundleStatus::Submitted { bundle_id })
    }

    /// Log current statistics
    pub fn log_stats(&self) {
        let stats = &self.stats;
        let entries = stats.entries_received.load(Ordering::Relaxed);
        let txs = stats.transactions_parsed.load(Ordering::Relaxed);
        let swaps = stats.swaps_detected.load(Ordering::Relaxed);
        let opps = stats.opportunities_found.load(Ordering::Relaxed);
        let submitted = stats.bundles_submitted.load(Ordering::Relaxed);
        let accepted = stats.bundles_accepted.load(Ordering::Relaxed);
        let rejected = stats.bundles_rejected.load(Ordering::Relaxed);
        let profit = stats.total_profit_lamports.load(Ordering::Relaxed);
        let slot = stats.last_slot.load(Ordering::Relaxed);

        let accept_rate = if submitted > 0 {
            (accepted as f64 / submitted as f64) * 100.0
        } else {
            0.0
        };

        tracing::info!(
            "MEV STATS: slot={} entries={} txs={} swaps={} opps={} submitted={} accepted={} rejected={} rate={:.1}% profit={:.6} SOL",
            slot, entries, txs, swaps, opps, submitted, accepted, rejected, accept_rate,
            profit as f64 / 1e9
        );
    }
}

/// Spawn the pipeline as a background task with stats logging
pub async fn spawn_pipeline(
    config: PipelineConfig,
    pool_states: Arc<PoolStateCache>,
    pool_registry: Option<Arc<DynamicPoolRegistry>>,
    payer: Arc<Keypair>,
    flash_loan_reserves: HashMap<solana_sdk::pubkey::Pubkey, FlashLoanReserve>,
    lookup_tables: Vec<AddressLookupTableAccount>,
    jito_client: Arc<JitoClient>,
    blockhash_rx: mpsc::Receiver<BlockhashUpdate>,
    tip_floor_cache: Option<Arc<TipFloorCache>>,
) -> Result<Arc<PipelineStats>> {
    let mut pipeline = MevPipeline::new(
        config,
        pool_states,
        pool_registry,
        payer,
        flash_loan_reserves,
        lookup_tables,
        jito_client,
        tip_floor_cache,
    );

    let stats = pipeline.stats();

    // Spawn blockhash update task
    let blockhash_arc = Arc::clone(&pipeline.blockhash);
    tokio::spawn(async move {
        let mut rx = blockhash_rx;
        while let Some(update) = rx.recv().await {
            let mut guard = blockhash_arc.write().await;
            *guard = (update.hash, update.slot);
        }
    });

    // Spawn stats logging task
    let stats_clone = Arc::clone(&stats);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let s = &stats_clone;
            let entries = s.entries_received.load(Ordering::Relaxed);
            let txs = s.transactions_parsed.load(Ordering::Relaxed);
            let swaps = s.swaps_detected.load(Ordering::Relaxed);
            let opps = s.opportunities_found.load(Ordering::Relaxed);
            let submitted = s.bundles_submitted.load(Ordering::Relaxed);
            let accepted = s.bundles_accepted.load(Ordering::Relaxed);
            let profit = s.total_profit_lamports.load(Ordering::Relaxed);

            let rate = if submitted > 0 {
                (accepted as f64 / submitted as f64) * 100.0
            } else {
                0.0
            };

            tracing::info!(
                "MEV PIPELINE: entries={} txs={} swaps={} opps={} sent={} accepted={} rate={:.1}% profit={:.6} SOL",
                entries, txs, swaps, opps, submitted, accepted, rate, profit as f64 / 1e9
            );
        }
    });

    // Spawn bundle result subscriber to track actual bundle outcomes from Jito
    let jito_for_results = Arc::clone(&pipeline.jito_client);
    let stats_for_results = Arc::clone(&stats);
    tokio::spawn(async move {
        run_bundle_result_subscriber(jito_for_results, stats_for_results).await;
    });

    // Spawn main pipeline loop
    tokio::spawn(async move {
        loop {
            match pipeline.run().await {
                Ok(()) => {
                    tracing::info!("Pipeline completed normally");
                    break;
                }
                Err(e) => {
                    tracing::warn!("Pipeline error: {}, reconnecting in 3s...", e);
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    });

    Ok(stats)
}

/// Run bundle result subscriber to track actual bundle outcomes from Jito
async fn run_bundle_result_subscriber(
    jito_client: Arc<JitoClient>,
    stats: Arc<PipelineStats>,
) {
    loop {
        match jito_client.subscribe_bundle_results().await {
            Ok(mut stream) => {
                tracing::info!("Bundle result subscriber connected");

                while let Ok(Some(result)) = stream.message().await {
                    let status: BundleStatus = result.into();
                    match &status {
                        BundleStatus::Accepted { bundle_id, slot, validator } => {
                            stats.bundles_accepted.fetch_add(1, Ordering::Relaxed);
                            tracing::info!(
                                bundle_id = %bundle_id,
                                slot = slot,
                                validator = %validator,
                                "🎯 Bundle ACCEPTED by validator!"
                            );
                        }
                        BundleStatus::Finalized { bundle_id } => {
                            tracing::info!(bundle_id = %bundle_id, "✅ Bundle FINALIZED");
                        }
                        BundleStatus::Processed { bundle_id, slot, .. } => {
                            tracing::debug!(bundle_id = %bundle_id, slot = slot, "Bundle processed");
                        }
                        BundleStatus::Rejected { bundle_id, reason } => {
                            stats.bundles_rejected.fetch_add(1, Ordering::Relaxed);
                            tracing::warn!(
                                bundle_id = %bundle_id,
                                reason = %reason,
                                "❌ Bundle REJECTED"
                            );
                        }
                        BundleStatus::Dropped { bundle_id, reason } => {
                            stats.bundles_rejected.fetch_add(1, Ordering::Relaxed);
                            tracing::warn!(
                                bundle_id = %bundle_id,
                                reason = %reason,
                                "⚠️ Bundle DROPPED"
                            );
                        }
                        BundleStatus::Submitted { .. } | BundleStatus::Unknown { .. } => {}
                    }
                }

                tracing::warn!("Bundle result stream ended, reconnecting in 5s...");
            }
            Err(e) => {
                tracing::warn!("Bundle result subscriber error: {}, reconnecting in 5s...", e);
            }
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.max_opportunity_queue, 100);
        assert_eq!(config.submission_timeout, Duration::from_secs(5));
    }
}
