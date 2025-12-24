//! Geyser-based MEV Pipeline
//!
//! Main processing loop: Geyser TransactionUpdates → parse → evaluate → build → submit
//! This replaces the shredstream proxy with direct Geyser plugin access.

use super::backrun::{BackrunBundleBuilder, BundleBuilderConfig, SandwichBundle};
use super::evaluator::{MevEvaluator, MevOpportunity, PoolInfo, SandwichOpportunity};
use super::parser::parse_transaction;
use crate::amm_math::PoolStateCache;
use crate::executor::FlashLoanReserve;
use crate::geyser::{self, GeyserClient, GeyserConfig, TransactionUpdate};
use crate::jito::{BundleStatus, JitoClient};
use crate::pool_discovery::DynamicPoolRegistry;
use anyhow::{Context, Result};
use solana_message::AddressLookupTableAccount;
use solana_message::v0::LoadedAddresses;
use solana_sdk::hash::Hash;
use solana_sdk::message::{v0, Message, VersionedMessage};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature};
use solana_sdk::transaction::VersionedTransaction;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};

/// Pipeline statistics
#[derive(Debug, Default)]
pub struct GeyserPipelineStats {
    pub transactions_received: AtomicU64,
    pub vote_txs_skipped: AtomicU64,
    pub transactions_parsed: AtomicU64,
    pub swaps_detected: AtomicU64,
    pub opportunities_found: AtomicU64,
    pub bundles_built: AtomicU64,
    pub bundles_submitted: AtomicU64,
    pub bundles_accepted: AtomicU64,
    pub bundles_rejected: AtomicU64,
    pub bundles_finalized: AtomicU64,
    pub bundles_dropped: AtomicU64,
    pub total_profit_lamports: AtomicU64,
    pub last_slot: AtomicU64,
    pub reconnect_count: AtomicU64,
    // Evaluator filter stats
    pub eval_too_small: AtomicU64,
    pub eval_no_pool: AtomicU64,
    pub eval_no_route: AtomicU64,
    pub eval_below_threshold: AtomicU64,
    pub pools_known: AtomicU64,
    // Bundle result subscriber stats
    pub result_subscriber_connected: AtomicU64,
}

/// Pipeline configuration
#[derive(Clone, Debug)]
pub struct GeyserPipelineConfig {
    /// Geyser gRPC endpoint
    pub geyser_endpoint: String,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Request timeout
    pub request_timeout: Duration,
    /// Skip vote transactions (recommended)
    pub skip_votes: bool,
    /// Bundle builder config
    pub bundle_config: BundleBuilderConfig,
    /// Bundle submission timeout
    pub submission_timeout: Duration,
    /// Minimum time between submissions (rate limiting)
    pub min_submission_interval: Duration,
    /// Whether sandwich attacks are enabled
    pub sandwich_enabled: bool,
}

impl Default for GeyserPipelineConfig {
    fn default() -> Self {
        Self {
            geyser_endpoint: "http://localhost:10000".to_string(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            skip_votes: true,
            bundle_config: BundleBuilderConfig::default(),
            submission_timeout: Duration::from_secs(5),
            min_submission_interval: Duration::from_millis(200),
            sandwich_enabled: false,
        }
    }
}

impl GeyserPipelineConfig {
    pub fn to_geyser_config(&self) -> GeyserConfig {
        GeyserConfig {
            endpoint: self.geyser_endpoint.clone(),
            connect_timeout: self.connect_timeout,
            request_timeout: self.request_timeout,
        }
    }
}

/// The main MEV pipeline using Geyser
pub struct GeyserMevPipeline {
    config: GeyserPipelineConfig,
    evaluator: MevEvaluator,
    bundle_builder: BackrunBundleBuilder,
    jito_client: Arc<JitoClient>,
    blockhash: Arc<RwLock<(Hash, u64)>>, // (hash, slot)
    stats: Arc<GeyserPipelineStats>,
}

impl GeyserMevPipeline {
    pub fn new(
        config: GeyserPipelineConfig,
        pool_states: Arc<PoolStateCache>,
        pool_registry: Option<Arc<DynamicPoolRegistry>>,
        payer: Arc<Keypair>,
        flash_loan_reserves: HashMap<Pubkey, FlashLoanReserve>,
        lookup_tables: Vec<AddressLookupTableAccount>,
        jito_client: Arc<JitoClient>,
    ) -> Self {
        let evaluator = MevEvaluator::new(pool_states, pool_registry, config.sandwich_enabled);
        let bundle_builder = BackrunBundleBuilder::new(
            config.bundle_config.clone(),
            payer,
            flash_loan_reserves,
            lookup_tables,
        );

        Self {
            config,
            evaluator,
            bundle_builder,
            jito_client,
            blockhash: Arc::new(RwLock::new((Hash::default(), 0))),
            stats: Arc::new(GeyserPipelineStats::default()),
        }
    }

    /// Get pipeline statistics
    pub fn stats(&self) -> Arc<GeyserPipelineStats> {
        Arc::clone(&self.stats)
    }

    /// Update the blockhash used for transaction building
    pub async fn update_blockhash(&self, hash: Hash, slot: u64) {
        let mut guard = self.blockhash.write().await;
        *guard = (hash, slot);
    }

    /// Get the blockhash Arc for external updates
    pub fn blockhash_arc(&self) -> Arc<RwLock<(Hash, u64)>> {
        Arc::clone(&self.blockhash)
    }

    /// Register a pool for arbitrage routing
    pub fn register_pool(&mut self, info: PoolInfo) {
        self.evaluator.register_pool(info);
    }

    /// Sync pools from dynamic registry
    pub fn sync_pools_from_registry(&mut self) {
        self.evaluator.sync_from_registry();
    }

    /// Run the pipeline (main loop)
    pub async fn run(&mut self) -> Result<()> {
        tracing::info!(
            "Starting Geyser MEV pipeline, connecting to {}",
            self.config.geyser_endpoint
        );

        // Connect to Geyser
        let client = GeyserClient::new(self.config.to_geyser_config())
            .await
            .context("Failed to connect to Geyser")?;

        // Subscribe to transaction updates
        let mut stream = client
            .subscribe_transactions()
            .await
            .context("Failed to subscribe to Geyser transactions")?;

        tracing::info!("Geyser transaction subscription established");

        let mut last_submission = Instant::now() - self.config.min_submission_interval;
        let mut last_registry_sync = Instant::now();
        let mut last_eval_log = Instant::now();
        let registry_sync_interval = Duration::from_secs(2); // Sync more frequently (was 5)
        let eval_log_interval = Duration::from_secs(10); // Log more frequently (was 30)

        // Initial sync from registry
        self.sync_pools_from_registry();
        tracing::info!(
            "Initial pool sync: {} pools, {} routes",
            self.evaluator.known_pool_count(),
            self.evaluator.route_count()
        );

        // Main processing loop
        while let Some(timestamped_update) = stream.message().await? {
            // Periodic sync from registry (picks up newly discovered pools)
            if last_registry_sync.elapsed() > registry_sync_interval {
                self.sync_pools_from_registry();
                last_registry_sync = Instant::now();
                
                // Update pool count stat
                self.stats.pools_known.store(self.evaluator.known_pool_count() as u64, Ordering::Relaxed);
            }

            // Periodic evaluator stats logging
            if last_eval_log.elapsed() > eval_log_interval {
                let es = self.evaluator.stats();
                tracing::info!(
                    "EVALUATOR: pools={} routes={} | {}",
                    self.evaluator.known_pool_count(),
                    self.evaluator.route_count(),
                    self.evaluator.stats_summary()
                );
                last_eval_log = Instant::now();
            }

            let Some(tx_update) = timestamped_update.transaction else {
                continue;
            };

            let slot = tx_update.slot;
            self.stats.last_slot.store(slot, Ordering::Relaxed);
            self.stats.transactions_received.fetch_add(1, Ordering::Relaxed);

            // Skip vote transactions
            if self.config.skip_votes && tx_update.is_vote {
                self.stats.vote_txs_skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            let received_at = Instant::now();

            // Convert Geyser transaction to VersionedTransaction
            let Some(confirmed_tx) = &tx_update.tx else {
                continue;
            };

            let (versioned_tx, loaded_addresses) = match convert_geyser_transaction(confirmed_tx) {
                Some(result) => result,
                None => continue,
            };

            self.stats.transactions_parsed.fetch_add(1, Ordering::Relaxed);

            // Parse for swaps
            let loaded_ref = loaded_addresses.as_ref();
            if let Some(tx_swaps) = parse_transaction(&versioned_tx, slot, received_at, loaded_ref) {
                self.stats
                    .swaps_detected
                    .fetch_add(tx_swaps.swaps.len() as u64, Ordering::Relaxed);

                // Evaluate for MEV opportunities
                let opportunities = self.evaluator.evaluate(&tx_swaps);

                for opp in opportunities {
                    self.stats.opportunities_found.fetch_add(1, Ordering::Relaxed);

                    // Rate limit submissions
                    let elapsed = last_submission.elapsed();
                    if elapsed < self.config.min_submission_interval {
                        tokio::time::sleep(self.config.min_submission_interval - elapsed).await;
                    }

                    // Build and submit bundle
                    match self.process_opportunity(&opp).await {
                        Ok(status) => {
                            last_submission = Instant::now();
                            match status {
                                BundleStatus::Submitted { .. } => {
                                    tracing::debug!(opp_id = %opp.id, "Bundle submitted");
                                }
                                BundleStatus::Accepted { .. }
                                | BundleStatus::Finalized { .. }
                                | BundleStatus::Processed { .. } => {
                                    self.stats.bundles_accepted.fetch_add(1, Ordering::Relaxed);
                                    self.stats.total_profit_lamports.fetch_add(
                                        opp.expected_profit_lamports,
                                        Ordering::Relaxed,
                                    );
                                    tracing::info!(
                                        opp_id = %opp.id,
                                        profit = opp.expected_profit_lamports,
                                        "Bundle accepted!"
                                    );
                                }
                                BundleStatus::Rejected { reason, .. } => {
                                    self.stats.bundles_rejected.fetch_add(1, Ordering::Relaxed);
                                    tracing::debug!(
                                        opp_id = %opp.id,
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
                        Err(e) => {
                            tracing::debug!(
                                opp_id = %opp.id,
                                error = %e,
                                "Failed to process opportunity"
                            );
                        }
                    }
                }
            }
        }

        tracing::warn!("Geyser stream ended");
        Ok(())
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

        let bundle_id = self
            .jito_client
            .send_versioned_bundle(bundle.transactions())
            .await?;

        tracing::debug!(bundle_id = %bundle_id, "Bundle submitted to Jito");

        Ok(BundleStatus::Submitted { bundle_id })
    }

    /// Log current statistics
    pub fn log_stats(&self) {
        let s = &self.stats;
        let received = s.transactions_received.load(Ordering::Relaxed);
        let votes_skipped = s.vote_txs_skipped.load(Ordering::Relaxed);
        let parsed = s.transactions_parsed.load(Ordering::Relaxed);
        let swaps = s.swaps_detected.load(Ordering::Relaxed);
        let opps = s.opportunities_found.load(Ordering::Relaxed);
        let submitted = s.bundles_submitted.load(Ordering::Relaxed);
        let accepted = s.bundles_accepted.load(Ordering::Relaxed);
        let rejected = s.bundles_rejected.load(Ordering::Relaxed);
        let profit = s.total_profit_lamports.load(Ordering::Relaxed);
        let slot = s.last_slot.load(Ordering::Relaxed);
        let reconnects = s.reconnect_count.load(Ordering::Relaxed);

        let rate = if submitted > 0 {
            (accepted as f64 / submitted as f64) * 100.0
        } else {
            0.0
        };

        tracing::info!(
            "GEYSER MEV: slot={} rx={} (votes_skip={}) parsed={} swaps={} opps={} sent={} ok={} fail={} rate={:.1}% profit={:.6} SOL reconnects={}",
            slot, received, votes_skipped, parsed, swaps, opps, submitted, accepted, rejected, rate,
            profit as f64 / 1e9, reconnects
        );
    }
}

/// Convert Geyser's ConfirmedTransaction to VersionedTransaction
fn convert_geyser_transaction(
    confirmed_tx: &geyser::proto::storage::confirmed_block::ConfirmedTransaction,
) -> Option<(VersionedTransaction, Option<LoadedAddresses>)> {
    let transaction = confirmed_tx.transaction.as_ref()?;
    let message = transaction.message.as_ref()?;

    // Parse signatures
    let signatures: Vec<Signature> = transaction
        .signatures
        .iter()
        .filter_map(|sig_bytes| {
            if sig_bytes.len() == 64 {
                let mut arr = [0u8; 64];
                arr.copy_from_slice(sig_bytes);
                Some(Signature::from(arr))
            } else {
                None
            }
        })
        .collect();

    if signatures.is_empty() {
        return None;
    }

    // Parse account keys
    let account_keys: Vec<Pubkey> = message
        .account_keys
        .iter()
        .filter_map(|key_bytes| {
            if key_bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(key_bytes);
                Some(Pubkey::new_from_array(arr))
            } else {
                None
            }
        })
        .collect();

    // Parse recent blockhash
    if message.recent_blockhash.len() != 32 {
        return None;
    }
    let mut blockhash_arr = [0u8; 32];
    blockhash_arr.copy_from_slice(&message.recent_blockhash);
    let recent_blockhash = Hash::new_from_array(blockhash_arr);

    // Parse instructions using the correct type from solana_message
    use solana_message::compiled_instruction::CompiledInstruction;
    let instructions: Vec<CompiledInstruction> = message
        .instructions
        .iter()
        .map(|ix| CompiledInstruction {
            program_id_index: ix.program_id_index as u8,
            accounts: ix.accounts.clone(),
            data: ix.data.clone(),
        })
        .collect();

    // Check if this is a v0 message (has address table lookups)
    let has_lookups = !message.address_table_lookups.is_empty();

    // Parse loaded addresses from meta
    let loaded_addresses = if let Some(meta) = &confirmed_tx.meta {
        let writable: Vec<Pubkey> = meta
            .loaded_writable_addresses
            .iter()
            .filter_map(|b| {
                if b.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(b);
                    Some(Pubkey::new_from_array(arr))
                } else {
                    None
                }
            })
            .collect();

        let readonly: Vec<Pubkey> = meta
            .loaded_readonly_addresses
            .iter()
            .filter_map(|b| {
                if b.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(b);
                    Some(Pubkey::new_from_array(arr))
                } else {
                    None
                }
            })
            .collect();

        if writable.is_empty() && readonly.is_empty() {
            None
        } else {
            Some(LoadedAddresses { writable, readonly })
        }
    } else {
        None
    };

    // Build the VersionedMessage
    let versioned_message = if has_lookups {
        // V0 message with address lookups
        let address_table_lookups: Vec<solana_sdk::message::v0::MessageAddressTableLookup> =
            message
                .address_table_lookups
                .iter()
                .filter_map(|lookup| {
                    if lookup.account_key.len() != 32 {
                        return None;
                    }
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&lookup.account_key);
                    Some(solana_sdk::message::v0::MessageAddressTableLookup {
                        account_key: Pubkey::new_from_array(arr),
                        writable_indexes: lookup.writable_indexes.clone(),
                        readonly_indexes: lookup.readonly_indexes.clone(),
                    })
                })
                .collect();

        // For v0 messages, we need to construct the header
        let header = message.header.as_ref().map(|h| solana_sdk::message::MessageHeader {
            num_required_signatures: h.num_required_signatures as u8,
            num_readonly_signed_accounts: h.num_readonly_signed_accounts as u8,
            num_readonly_unsigned_accounts: h.num_readonly_unsigned_accounts as u8,
        }).unwrap_or_default();

        let v0_message = v0::Message {
            header,
            account_keys,
            recent_blockhash,
            instructions,
            address_table_lookups,
        };

        VersionedMessage::V0(v0_message)
    } else {
        // Legacy message
        let header = message.header.as_ref().map(|h| solana_sdk::message::MessageHeader {
            num_required_signatures: h.num_required_signatures as u8,
            num_readonly_signed_accounts: h.num_readonly_signed_accounts as u8,
            num_readonly_unsigned_accounts: h.num_readonly_unsigned_accounts as u8,
        }).unwrap_or_default();

        let legacy_message = Message {
            header,
            account_keys,
            recent_blockhash,
            instructions,
        };

        VersionedMessage::Legacy(legacy_message)
    };

    Some((
        VersionedTransaction {
            signatures,
            message: versioned_message,
        },
        loaded_addresses,
    ))
}

/// Spawn the Geyser pipeline as a background task with stats logging
pub async fn spawn_geyser_pipeline(
    config: GeyserPipelineConfig,
    pool_states: Arc<PoolStateCache>,
    pool_registry: Option<Arc<DynamicPoolRegistry>>,
    payer: Arc<Keypair>,
    flash_loan_reserves: HashMap<Pubkey, FlashLoanReserve>,
    lookup_tables: Vec<AddressLookupTableAccount>,
    jito_client: Arc<JitoClient>,
) -> Result<(Arc<GeyserPipelineStats>, Arc<RwLock<(Hash, u64)>>)> {
    // Clone jito_client for the bundle result subscriber before passing to pipeline
    let jito_for_results = Arc::clone(&jito_client);

    let mut pipeline = GeyserMevPipeline::new(
        config,
        pool_states,
        pool_registry,
        payer,
        flash_loan_reserves,
        lookup_tables,
        jito_client,
    );

    let stats = pipeline.stats();
    let blockhash_arc = pipeline.blockhash_arc();

    // Spawn stats logging task
    let stats_clone = Arc::clone(&stats);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let s = &stats_clone;
            let received = s.transactions_received.load(Ordering::Relaxed);
            let votes_skipped = s.vote_txs_skipped.load(Ordering::Relaxed);
            let swaps = s.swaps_detected.load(Ordering::Relaxed);
            let opps = s.opportunities_found.load(Ordering::Relaxed);
            let submitted = s.bundles_submitted.load(Ordering::Relaxed);
            let accepted = s.bundles_accepted.load(Ordering::Relaxed);
            let profit = s.total_profit_lamports.load(Ordering::Relaxed);
            let slot = s.last_slot.load(Ordering::Relaxed);

            let rate = if submitted > 0 {
                (accepted as f64 / submitted as f64) * 100.0
            } else {
                0.0
            };

            tracing::info!(
                "GEYSER PIPELINE: slot={} rx={} (votes_skip={}) swaps={} opps={} sent={} ok={} rate={:.1}% profit={:.6} SOL",
                slot, received, votes_skipped, swaps, opps, submitted, accepted, rate, profit as f64 / 1e9
            );
        }
    });

    // Spawn Geyser blockhash updater
    let blockhash_clone = Arc::clone(&blockhash_arc);
    let geyser_config = GeyserConfig {
        endpoint: pipeline.config.geyser_endpoint.clone(),
        connect_timeout: pipeline.config.connect_timeout,
        request_timeout: pipeline.config.request_timeout,
    };
    tokio::spawn(async move {
        loop {
            match run_blockhash_updater(&geyser_config, &blockhash_clone).await {
                Ok(()) => break,
                Err(e) => {
                    tracing::warn!("Blockhash updater error: {}, reconnecting in 3s...", e);
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    });

    // Spawn bundle result subscriber to track actual bundle outcomes
    let stats_for_results = Arc::clone(&stats);
    tokio::spawn(async move {
        run_bundle_result_subscriber(jito_for_results, stats_for_results).await;
    });

    // Spawn main pipeline loop with reconnection
    let stats_for_reconnect = Arc::clone(&stats);
    tokio::spawn(async move {
        loop {
            match pipeline.run().await {
                Ok(()) => {
                    tracing::info!("Geyser pipeline completed normally");
                    break;
                }
                Err(e) => {
                    stats_for_reconnect.reconnect_count.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!("Geyser pipeline error: {}, reconnecting in 3s...", e);
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    });

    Ok((stats, blockhash_arc))
}

/// Run bundle result subscriber to track actual bundle outcomes from Jito
async fn run_bundle_result_subscriber(
    jito_client: Arc<JitoClient>,
    stats: Arc<GeyserPipelineStats>,
) {
    loop {
        match jito_client.subscribe_bundle_results().await {
            Ok(mut stream) => {
                stats.result_subscriber_connected.store(1, Ordering::Relaxed);
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
                                "Bundle ACCEPTED by validator"
                            );
                        }
                        BundleStatus::Finalized { bundle_id } => {
                            stats.bundles_finalized.fetch_add(1, Ordering::Relaxed);
                            tracing::info!(bundle_id = %bundle_id, "Bundle FINALIZED");
                        }
                        BundleStatus::Processed { bundle_id, slot, .. } => {
                            tracing::debug!(bundle_id = %bundle_id, slot = slot, "Bundle processed");
                        }
                        BundleStatus::Rejected { bundle_id, reason } => {
                            stats.bundles_rejected.fetch_add(1, Ordering::Relaxed);
                            tracing::warn!(
                                bundle_id = %bundle_id,
                                reason = %reason,
                                "Bundle REJECTED"
                            );
                        }
                        BundleStatus::Dropped { bundle_id, reason } => {
                            stats.bundles_dropped.fetch_add(1, Ordering::Relaxed);
                            tracing::warn!(
                                bundle_id = %bundle_id,
                                reason = %reason,
                                "Bundle DROPPED"
                            );
                        }
                        BundleStatus::Submitted { .. } | BundleStatus::Unknown { .. } => {}
                    }
                }

                stats.result_subscriber_connected.store(0, Ordering::Relaxed);
                tracing::warn!("Bundle result stream ended, reconnecting in 5s...");
            }
            Err(e) => {
                stats.result_subscriber_connected.store(0, Ordering::Relaxed);
                tracing::warn!("Bundle result subscriber error: {}, reconnecting in 5s...", e);
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Run blockhash updater from Geyser block stream
async fn run_blockhash_updater(
    config: &GeyserConfig,
    blockhash: &Arc<RwLock<(Hash, u64)>>,
) -> Result<()> {
    let client = GeyserClient::new(config.clone()).await?;
    let mut stream = client.subscribe_blocks().await?;

    tracing::info!("Geyser blockhash updater connected");

    while let Some(update) = stream.message().await? {
        if let Some(block) = update.block_update {
            if let Ok(hash) = block.blockhash.parse::<Hash>() {
                let mut guard = blockhash.write().await;
                *guard = (hash, block.slot);
                tracing::debug!(slot = block.slot, "Updated blockhash from Geyser");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_geyser_pipeline_config_default() {
        let config = GeyserPipelineConfig::default();
        assert_eq!(config.geyser_endpoint, "http://localhost:10000");
        assert!(config.skip_votes);
    }
}
