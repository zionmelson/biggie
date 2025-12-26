//! Unified MEV Pipeline - Tightly Coupled ShredStream + Geyser
//!
//! This pipeline integrates ShredStream (early TX detection) and Geyser (ground truth)
//! as DEPENDENT systems that work together for optimal MEV extraction.
//!
//! # Key Design Principles
//!
//! 1. **ShredStream provides early warning** - Detects victim TXs before execution
//! 2. **Geyser provides ground truth** - Confirms state after execution
//! 3. **Predictive state bridges the gap** - Simulates pending swaps for accurate evaluation
//! 4. **Neither source works alone** - They are tightly coupled and interdependent
//!
//! # Data Flow
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                       UNIFIED MEV PIPELINE                              │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                         │
//! │  SHREDSTREAM TASK                    GEYSER TASK                        │
//! │  ═══════════════                     ══════════                         │
//! │  • Subscribe entries                 • Subscribe transactions           │
//! │  • Parse transactions                • Subscribe accounts (vaults)      │
//! │  • Detect swaps EARLY                • Subscribe slots                  │
//! │         │                                   │                           │
//! │         ▼                                   ▼                           │
//! │  ┌─────────────────────────────────────────────────────────────────┐   │
//! │  │              PREDICTIVE STATE MANAGER (shared)                   │   │
//! │  │  • Adds pending swaps ◄── ShredStream                           │   │
//! │  │  • Confirms/removes pending ◄── Geyser TX updates               │   │
//! │  │  • Updates confirmed state ◄── Geyser account updates           │   │
//! │  │  • Computes predicted state = confirmed + pending               │   │
//! │  └─────────────────────────────────────────────────────────────────┘   │
//! │                              │                                          │
//! │                              ▼                                          │
//! │  ┌─────────────────────────────────────────────────────────────────┐   │
//! │  │              MEV EVALUATOR (uses predicted state)                │   │
//! │  │  • Evaluates opportunities against PREDICTED state              │   │
//! │  │  • Simulates victim impact (already in prediction)              │   │
//! │  │  • Finds cross-pool arb routes                                  │   │
//! │  │  • Calculates profit with state prediction                      │   │
//! │  └─────────────────────────────────────────────────────────────────┘   │
//! │                              │                                          │
//! │                              ▼                                          │
//! │  ┌─────────────────────────────────────────────────────────────────┐   │
//! │  │              BUNDLE BUILDER & JITO SUBMISSION                    │   │
//! │  └─────────────────────────────────────────────────────────────────┘   │
//! │                                                                         │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```

use crate::amm_math::CurveType;
use crate::geyser::{GeyserClient, GeyserConfig, SlotStatus};
use crate::jito::{BundleStatus, JitoClient};
use crate::pool_discovery::parse_pool_account;
use crate::predictive_state::{PoolPredictiveState, PredictiveStateManager};
use crate::shredstream::backrun::{BackrunBundleBuilder, BundleBuilderConfig};
use crate::shredstream::evaluator::{MevEvaluator, MevOpportunity};
use crate::shredstream::parser::{parse_transaction, TransactionSwaps};
use crate::shredstream::programs::{
    DexType, METEORA_DAMM, METEORA_DLMM, ORCA_WHIRLPOOL, PANCAKESWAP_CLMM, RAYDIUM_AMM, RAYDIUM_CLMM,
};
use crate::tip_monitor::TipFloorCache;
use crate::turbine::{
    TurbineReceiver, TurbineConfig, TurbineStatsSnapshot,
    AssembledEntries, run_entry_assembler,
    fec::FecAssemblerConfig,
};
use crate::config::TurbineUdpConfig;

use anyhow::{Context, Result};
use bincode::Options;
#[allow(deprecated)]
use solana_entry::entry::Entry as SolanaEntry;
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature};
use solana_sdk::transaction::VersionedTransaction;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tonic::transport::Channel;

// Import shredstream proxy client (used as fallback when Turbine disabled)
use crate::jito::shredstream::{
    shredstream_proxy_client::ShredstreamProxyClient, SubscribeEntriesRequest,
};

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Configuration for the unified pipeline
#[derive(Clone, Debug)]
pub struct UnifiedPipelineConfig {
    // === Turbine UDP settings (primary - lowest latency) ===
    /// Whether to use direct Turbine UDP (true) or ShredStream proxy (false)
    pub use_turbine_udp: bool,
    /// Turbine UDP configuration
    pub turbine_config: TurbineUdpConfig,
    
    // === ShredStream settings (fallback) ===
    /// ShredStream proxy gRPC endpoint (only used if use_turbine_udp = false)
    pub shredstream_endpoint: String,

    // === Geyser settings ===
    /// Geyser gRPC endpoint
    pub geyser_endpoint: String,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Request timeout
    pub request_timeout: Duration,
    /// Skip vote transactions
    pub skip_votes: bool,

    // === Bundle settings ===
    pub bundle_config: BundleBuilderConfig,
    /// Bundle submission timeout
    pub submission_timeout: Duration,
    /// Minimum time between submissions (rate limiting)
    pub min_submission_interval: Duration,
}

impl Default for UnifiedPipelineConfig {
    fn default() -> Self {
        Self {
            use_turbine_udp: true, // Default to Turbine UDP for lowest latency
            turbine_config: TurbineUdpConfig::default(),
            shredstream_endpoint: "http://127.0.0.1:10000".to_string(),
            geyser_endpoint: "http://localhost:10000".to_string(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            skip_votes: true,
            bundle_config: BundleBuilderConfig::default(),
            submission_timeout: Duration::from_secs(5),
            min_submission_interval: Duration::from_millis(200),
        }
    }
}

// =============================================================================
// PIPELINE STATISTICS
// =============================================================================

/// Comprehensive statistics for the unified pipeline
#[derive(Debug, Default)]
pub struct UnifiedPipelineStats {
    // === Turbine UDP stats (primary source) ===
    pub turbine_packets_received: AtomicU64,
    pub turbine_data_shreds: AtomicU64,
    pub turbine_coding_shreds: AtomicU64,
    pub turbine_fec_completed: AtomicU64,
    pub turbine_entries_assembled: AtomicU64,
    
    // === ShredStream stats (fallback) ===
    pub shredstream_entries_received: AtomicU64,
    pub shredstream_txs_parsed: AtomicU64,
    pub shredstream_swaps_detected: AtomicU64,
    pub shredstream_reconnects: AtomicU64,

    // === Geyser stats ===
    pub geyser_txs_received: AtomicU64,
    pub geyser_account_updates: AtomicU64,
    pub geyser_pool_updates: AtomicU64,
    pub geyser_slot_updates: AtomicU64,
    pub geyser_votes_skipped: AtomicU64,
    pub geyser_reconnects: AtomicU64,

    // === Predictive state stats ===
    pub pending_swaps_added: AtomicU64,
    pub pending_swaps_confirmed: AtomicU64,
    pub pending_swaps_expired: AtomicU64,

    // === MEV stats ===
    pub opportunities_evaluated: AtomicU64,
    pub opportunities_found: AtomicU64,
    pub bundles_built: AtomicU64,
    pub bundles_submitted: AtomicU64,
    pub bundles_accepted: AtomicU64,
    pub bundles_rejected: AtomicU64,
    pub total_profit_lamports: AtomicU64,

    // === General ===
    pub last_slot: AtomicU64,
    pub current_pending_swaps: AtomicU64,
    pub tx_source_turbine: AtomicU64,
    pub tx_source_shredstream: AtomicU64,
}

impl UnifiedPipelineStats {
    pub fn summary(&self) -> String {
        let turbine_pkts = self.turbine_packets_received.load(Ordering::Relaxed);
        let shred_entries = self.shredstream_entries_received.load(Ordering::Relaxed);
        
        // Determine active source
        let source_str = if turbine_pkts > 0 {
            format!("TURBINE[pkts={} data={} code={} fec={} entries={}]",
                turbine_pkts,
                self.turbine_data_shreds.load(Ordering::Relaxed),
                self.turbine_coding_shreds.load(Ordering::Relaxed),
                self.turbine_fec_completed.load(Ordering::Relaxed),
                self.turbine_entries_assembled.load(Ordering::Relaxed))
        } else {
            format!("SHRED[entries={} txs={} swaps={}]",
                shred_entries,
                self.shredstream_txs_parsed.load(Ordering::Relaxed),
                self.shredstream_swaps_detected.load(Ordering::Relaxed))
        };
        
        format!(
            "{} GEYSER[txs={} accts={}] PENDING[add={} conf={} exp={} cur={}] MEV[opps={} submitted={} accepted={}] profit={:.6} SOL",
            source_str,
            self.geyser_txs_received.load(Ordering::Relaxed),
            self.geyser_account_updates.load(Ordering::Relaxed),
            self.pending_swaps_added.load(Ordering::Relaxed),
            self.pending_swaps_confirmed.load(Ordering::Relaxed),
            self.pending_swaps_expired.load(Ordering::Relaxed),
            self.current_pending_swaps.load(Ordering::Relaxed),
            self.opportunities_found.load(Ordering::Relaxed),
            self.bundles_submitted.load(Ordering::Relaxed),
            self.bundles_accepted.load(Ordering::Relaxed),
            self.total_profit_lamports.load(Ordering::Relaxed) as f64 / 1e9
        )
    }
}

// =============================================================================
// UNIFIED PIPELINE
// =============================================================================

/// Channel for sending opportunities to the bundle submission task
pub type OpportunityTx = mpsc::Sender<MevOpportunity>;
pub type OpportunityRx = mpsc::Receiver<MevOpportunity>;

/// The unified MEV pipeline with tightly coupled ShredStream + Geyser
pub struct UnifiedMevPipeline {
    config: UnifiedPipelineConfig,

    // === Shared State (the coupling point) ===
    /// Predictive state manager - THE SINGLE SOURCE OF TRUTH for pool state
    /// Also handles vault→pool mapping internally via vault_index
    predictive_state: Arc<PredictiveStateManager>,

    // === Components ===
    evaluator: RwLock<MevEvaluator>,
    bundle_builder: RwLock<BackrunBundleBuilder>,
    jito_client: Arc<JitoClient>,
    blockhash: Arc<RwLock<(Hash, u64)>>,

    // === Submission Channel ===
    /// Channel to send opportunities for async submission (non-blocking)
    opportunity_tx: OpportunityTx,

    // === Statistics ===
    stats: Arc<UnifiedPipelineStats>,
}

impl UnifiedMevPipeline {
    pub fn new(
        config: UnifiedPipelineConfig,
        tip_floor_cache: Option<Arc<TipFloorCache>>,
        payer: Arc<Keypair>,
        flash_loan_reserves: HashMap<Pubkey, crate::executor::FlashLoanReserve>,
        lookup_tables: Vec<solana_message::AddressLookupTableAccount>,
        jito_client: Arc<JitoClient>,
        opportunity_tx: OpportunityTx,
    ) -> Self {
        let predictive_state = Arc::new(PredictiveStateManager::new());

        // Derive anchor mints from configured flash loan reserves
        // This ensures we only try to flash loan tokens we have reserves for
        let anchor_mints: Vec<Pubkey> = flash_loan_reserves.keys().copied().collect();
        tracing::info!(
            "Derived {} anchor mints from flash loan reserves",
            anchor_mints.len()
        );

        // Create evaluator with PredictiveStateManager as the single source of truth
        let evaluator = MevEvaluator::new(Arc::clone(&predictive_state), anchor_mints);

        let mut bundle_builder = BackrunBundleBuilder::new(
            config.bundle_config.clone(),
            payer,
            flash_loan_reserves,
            lookup_tables,
        );

        // Wire tip floor cache for dynamic tipping
        if let Some(cache) = tip_floor_cache {
            bundle_builder.set_tip_floor_cache(cache);
        }

        Self {
            config,
            predictive_state,
            evaluator: RwLock::new(evaluator),
            bundle_builder: RwLock::new(bundle_builder),
            jito_client,
            blockhash: Arc::new(RwLock::new((Hash::default(), 0))),
            opportunity_tx,
            stats: Arc::new(UnifiedPipelineStats::default()),
        }
    }

    /// Send an opportunity for async submission (non-blocking)
    pub async fn submit_opportunity(&self, opp: MevOpportunity) {
        // Try to send - if channel is full, log and drop (better than blocking)
        if let Err(e) = self.opportunity_tx.try_send(opp) {
            match e {
                mpsc::error::TrySendError::Full(opp) => {
                    tracing::warn!(
                        opp_id = %opp.id,
                        "Opportunity channel full - dropping (submission backpressure)"
                    );
                }
                mpsc::error::TrySendError::Closed(_) => {
                    tracing::error!("Opportunity channel closed - submission task died!");
                }
            }
        }
    }

    /// Get the predictive state manager (for external integration)
    pub fn predictive_state(&self) -> Arc<PredictiveStateManager> {
        Arc::clone(&self.predictive_state)
    }

    /// Get pipeline statistics
    pub fn stats(&self) -> Arc<UnifiedPipelineStats> {
        Arc::clone(&self.stats)
    }

    /// Get predictive state statistics as a formatted string
    pub fn predictive_state_stats(&self) -> String {
        self.predictive_state.stats_summary()
    }

    /// Get blockhash Arc for external updates
    pub fn blockhash_arc(&self) -> Arc<RwLock<(Hash, u64)>> {
        Arc::clone(&self.blockhash)
    }

    /// Get Jito client for bundle result subscription
    pub fn jito_client(&self) -> Arc<JitoClient> {
        Arc::clone(&self.jito_client)
    }

    /// Update the blockhash
    pub async fn update_blockhash(&self, hash: Hash, slot: u64) {
        let mut guard = self.blockhash.write().await;
        *guard = (hash, slot);
    }

    // =========================================================================
    // SHREDSTREAM HANDLER (Early TX Detection)
    // =========================================================================

    /// Handle a ShredStream entry - parse transactions and detect swaps EARLY
    #[allow(deprecated)] // solana_entry::entry::Entry is marked for Agave Unstable API
    pub async fn handle_shredstream_entry(
        &self,
        slot: u64,
        entries: Vec<SolanaEntry>,
    ) -> Vec<TransactionSwaps> {
        let received_at = Instant::now();
        let mut all_swaps = Vec::new();

        for entry in entries {
            for transaction in entry.transactions {
                self.stats
                    .shredstream_txs_parsed
                    .fetch_add(1, Ordering::Relaxed);

                let versioned_tx: VersionedTransaction = transaction.into();
                let signature = versioned_tx.signatures.first().cloned().unwrap_or_default();

                // Parse for swaps
                if let Some(tx_swaps) = parse_transaction(&versioned_tx, slot, received_at, None) {
                    self.stats
                        .shredstream_swaps_detected
                        .fetch_add(tx_swaps.swaps.len() as u64, Ordering::Relaxed);

                    // Add each swap to predictive state as PENDING
                    for swap in &tx_swaps.swaps {
                        self.predictive_state
                            .on_shredstream_swap_detected(swap, slot, signature);
                        self.stats
                            .pending_swaps_added
                            .fetch_add(1, Ordering::Relaxed);
                    }

                    // Update current pending count
                    self.stats.current_pending_swaps.store(
                        self.predictive_state.total_pending_swaps() as u64,
                        Ordering::Relaxed,
                    );

                    all_swaps.push(tx_swaps);
                }
            }
        }

        all_swaps
    }

    // =========================================================================
    // GEYSER HANDLERS (Ground Truth)
    // =========================================================================

    /// Handle Geyser transaction update - confirms pending swaps
    ///
    /// Uses signature index for O(1) lookup when we tracked the swap via ShredStream.
    /// Falls back to re-parsing the transaction for swaps we didn't see early.
    ///
    /// The `tx_data` closure is only called if we need to fall back to re-parsing.
    pub fn handle_geyser_transaction_lazy<F>(
        &self,
        signature: Signature,
        _slot: u64, // Available for future slot-based validation
        success: bool,
        fallback_pools_fn: F,
    ) where
        F: FnOnce() -> Vec<Pubkey>,
    {
        self.stats
            .geyser_txs_received
            .fetch_add(1, Ordering::Relaxed);

        // Try fast path: use signature index (avoids re-parsing)
        if let Some(indexed_pools) = self
            .predictive_state
            .confirm_by_signature(&signature, success)
        {
            // Fast path succeeded - pools were confirmed via index
            if success && !indexed_pools.is_empty() {
                self.stats
                    .pending_swaps_confirmed
                    .fetch_add(1, Ordering::Relaxed);
            }
        } else {
            // Slow path: signature wasn't in our index (we didn't see it via ShredStream)
            // Now extract pools (lazy - only when needed)
            let fallback_pools = fallback_pools_fn();
            self.predictive_state.record_signature_index_miss();
            for pool in &fallback_pools {
                self.predictive_state
                    .on_geyser_transaction_confirmed(&signature, pool, success);
            }
            if success && !fallback_pools.is_empty() {
                self.stats
                    .pending_swaps_confirmed
                    .fetch_add(1, Ordering::Relaxed);
            }
        }

        // Update current pending count
        self.stats.current_pending_swaps.store(
            self.predictive_state.total_pending_swaps() as u64,
            Ordering::Relaxed,
        );
    }

    /// Handle Geyser account update - updates confirmed reserves
    pub fn handle_geyser_account_update(
        &self,
        pool: Pubkey,
        base_reserve: u64,
        quote_reserve: u64,
        slot: u64,
    ) {
        self.stats
            .geyser_account_updates
            .fetch_add(1, Ordering::Relaxed);
        self.predictive_state
            .on_geyser_reserves_update(&pool, base_reserve, quote_reserve, slot);
    }

    /// Handle Geyser vault account update - updates pool reserves via PSM's vault_index
    ///
    /// This is called when we receive an account update for a tracked vault.
    /// PSM looks up which pool this vault belongs to and updates the reserves accordingly.
    pub fn handle_geyser_vault_update(&self, vault_address: &Pubkey, balance: u64, slot: u64) {
        // PSM handles the vault→pool lookup and reserve update internally
        self.predictive_state
            .on_geyser_vault_address_update(vault_address, balance, slot);
    }

    /// Handle Geyser slot update
    pub fn handle_geyser_slot_update(&self, slot: u64, status: SlotStatus) {
        self.stats
            .geyser_slot_updates
            .fetch_add(1, Ordering::Relaxed);
        self.stats.last_slot.store(slot, Ordering::Relaxed);

        // On slot confirmation, we could trigger cleanup of old pending swaps
        if status == SlotStatus::Confirmed {
            // Pending swaps from 2+ slots ago are likely stale
            // The predictive_state manager handles this via TTL
        }
    }

    /// Handle Geyser DEX program account update - updates pool state (CLMM tick, sqrt_price, etc.)
    ///
    /// This replaces the separate `run_pool_state_updater` task, consolidating all Geyser
    /// subscriptions into the unified pipeline's single connection.
    ///
    /// Handles three types of accounts:
    /// 1. Pool accounts → updates pool state in PredictiveStateManager
    /// 2. Tick array accounts (CLMM) → updates TickArrayCache
    /// 3. Bin array accounts (DLMM) → updates BinArrayCache
    pub fn handle_geyser_pool_update(
        &self,
        address: Pubkey,
        program_id: &Pubkey,
        data: &[u8],
        slot: u64,
    ) {
        self.stats
            .geyser_pool_updates
            .fetch_add(1, Ordering::Relaxed);

        // First, try to process as a tick/bin array (fast path for cached accounts)
        // These are smaller and more frequently updated than pool accounts
        let tick_cache = self.predictive_state.tick_cache();
        if tick_cache.process_account_update(&address, data, slot) {
            return; // Was a registered tick array
        }

        let bin_cache = self.predictive_state.bin_cache();
        if bin_cache.process_account_update(&address, data, slot) {
            return; // Was a registered bin array
        }

        // Try to parse as a pool account
        let Some(pool) = parse_pool_account(address, program_id, data) else {
            return;
        };

        // Determine curve type from DEX type
        let curve = match pool.dex {
            DexType::OrcaWhirlpool | DexType::RaydiumClmm | DexType::PancakeSwapClmm => {
                CurveType::ConcentratedLiquidity
            }
            DexType::MeteoraDlmm => CurveType::Dlmm,
            _ => CurveType::ConstantProduct,
        };

        let fee_bps = pool.fee_bps.unwrap_or(25);
        let is_new_pool = !self.predictive_state.has_pool(&pool.address);

        // Register or update predictive state (single source of truth)
        if is_new_pool {
            // Create and register new pool in predictive state with metadata
            let mut pool_state = PoolPredictiveState::new(
                pool.address,
                pool.dex,
                pool.token_mint_a,
                pool.token_mint_b,
                0, // Reserves will be fetched via vault subscription
                0,
                pool.decimals_a,
                pool.decimals_b,
                fee_bps,
                curve,
                slot,
            );

            // Set metadata for swap instruction building
            pool_state.set_metadata(
                pool.token_vault_a,
                pool.token_vault_b,
                pool.amm_authority,
                pool.amm_config,
                pool.observation_state,
                pool.tick_arrays.clone(),
                pool.bin_array_bitmap_extension,
                pool.bin_arrays.clone(),
                pool.pool_authority,
            );

            self.predictive_state.register_pool(pool_state);
        } else {
            // Update metadata for existing pool
            self.predictive_state.update_pool_metadata(
                &pool.address,
                pool.token_vault_a,
                pool.token_vault_b,
                pool.amm_authority,
                pool.amm_config,
                pool.observation_state,
                pool.tick_arrays.clone(),
                pool.bin_array_bitmap_extension,
                pool.bin_arrays.clone(),
                pool.pool_authority,
            );
        }

        // Note: Vault registration is handled automatically by PSM during
        // register_pool() and update_pool_metadata() via vault_index

        // Update CLMM state if available (for both new and existing pools)
        if pool.spacing > 0 {
            self.predictive_state.update_clmm_state(
                &pool.address,
                pool.sqrt_price,
                pool.current_tick_or_bin,
                pool.spacing,
                pool.active_liquidity,
                slot,
            );
        }

        tracing::trace!(
            "POOL UPDATE: {} {:?} tick={} sqrt_price={:?} new={}",
            &pool.address.to_string()[..8],
            pool.dex,
            pool.current_tick_or_bin,
            pool.sqrt_price,
            is_new_pool
        );
    }

    // =========================================================================
    // MEV EVALUATION (Uses Predicted State)
    // =========================================================================

    /// Evaluate swaps for MEV opportunities using PREDICTED state
    ///
    /// This is the key function that benefits from state prediction.
    /// When ShredStream detects a victim TX, the predicted state already
    /// includes the victim's impact, allowing accurate profit calculation.
    pub async fn evaluate_opportunities(&self, tx_swaps: &TransactionSwaps) -> Vec<MevOpportunity> {
        // The evaluator uses PredictiveStateManager for accurate results.
        // The predictive_state has pending swaps factored in.
        // Using read lock since evaluate() now uses atomic stats
        let evaluator = self.evaluator.read().await;

        // Evaluate (the evaluator uses predicted state via PredictiveStateManager)
        let opportunities = evaluator.evaluate(tx_swaps);

        self.stats
            .opportunities_evaluated
            .fetch_add(tx_swaps.swaps.len() as u64, Ordering::Relaxed);
        self.stats
            .opportunities_found
            .fetch_add(opportunities.len() as u64, Ordering::Relaxed);

        opportunities
    }

    /// Process a single opportunity - build and submit bundle
    pub async fn process_opportunity(&self, opp: &MevOpportunity) -> Result<BundleStatus> {
        // Get current blockhash
        let (blockhash, _slot) = *self.blockhash.read().await;
        if blockhash == Hash::default() {
            anyhow::bail!("No blockhash available");
        }

        // Build bundle
        let builder = self.bundle_builder.read().await;
        let bundle = builder.build(opp, blockhash)?;
        drop(builder); // Release lock before network I/O

        self.stats.bundles_built.fetch_add(1, Ordering::Relaxed);

        tracing::info!(
            opp_id = %opp.id,
            bundle_id = %bundle.id,
            profit = opp.expected_profit_lamports,
            tip = bundle.tip_lamports,
            pending_swaps = self.predictive_state.total_pending_swaps(),
            "Submitting backrun bundle (with state prediction)"
        );

        // Submit to Jito
        self.stats.bundles_submitted.fetch_add(1, Ordering::Relaxed);

        let bundle_id = self
            .jito_client
            .send_versioned_bundle(bundle.transactions())
            .await?;

        Ok(BundleStatus::Submitted { bundle_id })
    }

    // =========================================================================
    // LOGGING
    // =========================================================================

    /// Log current statistics
    pub fn log_stats(&self) {
        tracing::info!("{}", self.stats.summary());

        // Also log predictive state stats
        let ps_stats = self.predictive_state.stats();
        tracing::debug!(
            "PREDICTIVE STATE: pools={} pending_total={} confirmed={} expired={}",
            ps_stats.pools_tracked,
            ps_stats.pending_swaps_total,
            ps_stats.pending_swaps_confirmed,
            ps_stats.pending_swaps_expired
        );
    }
}

// =============================================================================
// SPAWN UNIFIED PIPELINE
// =============================================================================

/// Spawn the unified pipeline with Turbine UDP (or ShredStream fallback) + Geyser tasks
///
/// Primary mode (Turbine UDP enabled):
/// - Turbine provides ultra-low-latency TX detection (~0.5-2ms)
/// - Geyser provides ground truth confirmation and account updates
///
/// Fallback mode (Turbine disabled):
/// - ShredStream proxy provides early TX detection (~5-10ms)
/// - Geyser provides ground truth confirmation
pub async fn spawn_unified_pipeline(
    config: UnifiedPipelineConfig,
    tip_floor_cache: Option<Arc<TipFloorCache>>,
    payer: Arc<Keypair>,
    flash_loan_reserves: HashMap<Pubkey, crate::executor::FlashLoanReserve>,
    lookup_tables: Vec<solana_message::AddressLookupTableAccount>,
    jito_client: Arc<JitoClient>,
) -> Result<(Arc<UnifiedMevPipeline>, Arc<UnifiedPipelineStats>)> {
    // Create channel for async opportunity submission (bounded to prevent OOM)
    let (opportunity_tx, opportunity_rx) = mpsc::channel::<MevOpportunity>(100);

    let pipeline = Arc::new(UnifiedMevPipeline::new(
        config.clone(),
        tip_floor_cache,
        payer,
        flash_loan_reserves,
        lookup_tables,
        jito_client,
        opportunity_tx,
    ));

    let stats = pipeline.stats();

    // Spawn bundle submission task (handles rate limiting without blocking entry processing)
    let pipeline_for_submit = Arc::clone(&pipeline);
    let submit_config = config.clone();
    tokio::spawn(async move {
        run_bundle_submission_task(pipeline_for_submit, opportunity_rx, submit_config).await;
    });

    // Spawn TX source task - either Turbine UDP (primary) or ShredStream (fallback)
    if config.use_turbine_udp {
        tracing::info!(
            bind_addr = %config.turbine_config.bind_addr,
            threads = config.turbine_config.receiver_threads,
            "Starting Turbine UDP receiver (ultra-low-latency mode)"
        );
        
        let pipeline_for_turbine = Arc::clone(&pipeline);
        let turbine_config = config.turbine_config.clone();
        let stats_for_turbine = Arc::clone(&stats);
        tokio::spawn(async move {
            run_turbine_task(pipeline_for_turbine, turbine_config, stats_for_turbine).await;
        });
    } else {
        tracing::info!(
            endpoint = %config.shredstream_endpoint,
            "Starting ShredStream proxy client (fallback mode)"
        );
        
        let pipeline_for_shred = Arc::clone(&pipeline);
        let shred_config = config.clone();
        tokio::spawn(async move {
            run_shredstream_task(pipeline_for_shred, shred_config).await;
        });
    }

    // Spawn Geyser tasks - ground truth for state confirmation
    let pipeline_for_geyser = Arc::clone(&pipeline);
    let geyser_config = config.clone();
    tokio::spawn(async move {
        run_geyser_tasks(pipeline_for_geyser, geyser_config).await;
    });

    // Spawn stats logging task
    let pipeline_for_log = Arc::clone(&pipeline);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            pipeline_for_log.log_stats();
        }
    });

    // Spawn bundle result subscriber to track actual bundle outcomes from Jito
    let jito_for_results = pipeline.jito_client();
    let stats_for_results = Arc::clone(&stats);
    tokio::spawn(async move {
        run_bundle_result_subscriber(jito_for_results, stats_for_results).await;
    });

    Ok((pipeline, stats))
}

/// Run bundle result subscriber to track actual bundle outcomes from Jito
async fn run_bundle_result_subscriber(
    jito_client: Arc<JitoClient>,
    stats: Arc<UnifiedPipelineStats>,
) {
    loop {
        match jito_client.subscribe_bundle_results().await {
            Ok(mut stream) => {
                tracing::info!("[Unified] Bundle result subscriber connected");

                while let Ok(Some(result)) = stream.message().await {
                    let status: BundleStatus = result.into();
                    match &status {
                        BundleStatus::Accepted {
                            bundle_id,
                            slot,
                            validator,
                        } => {
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
                        BundleStatus::Processed {
                            bundle_id, slot, ..
                        } => {
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

                tracing::warn!("[Unified] Bundle result stream ended, reconnecting in 5s...");
            }
            Err(e) => {
                tracing::warn!(
                    "[Unified] Bundle result subscriber error: {}, reconnecting in 5s...",
                    e
                );
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Bundle submission task - consumes opportunities from channel with smart rate limiting
///
/// This runs separately from entry processing so rate limiting doesn't block
/// the ShredStream loop from receiving and evaluating new entries.
///
/// OPTIMIZATION: High-profit opportunities skip rate limiting to maximize speed.
/// Rate limiting only applies to prevent spam on low-profit opportunities.
async fn run_bundle_submission_task(
    pipeline: Arc<UnifiedMevPipeline>,
    mut opportunity_rx: OpportunityRx,
    config: UnifiedPipelineConfig,
) {
    tracing::info!(
        "Bundle submission task started (base_rate_limit={}ms)",
        config.min_submission_interval.as_millis()
    );

    // High-profit threshold: skip rate limiting for opportunities above this
    const HIGH_PROFIT_THRESHOLD: u64 = 100_000; // 0.0001 SOL

    let mut last_submission = Instant::now() - config.min_submission_interval;

    while let Some(opp) = opportunity_rx.recv().await {
        // Smart rate limiting: high-profit opportunities skip the delay
        // This ensures we submit fast when it matters most
        let is_high_profit = opp.expected_profit_lamports >= HIGH_PROFIT_THRESHOLD;
        
        if !is_high_profit {
            let elapsed = last_submission.elapsed();
            if elapsed < config.min_submission_interval {
                tokio::time::sleep(config.min_submission_interval - elapsed).await;
            }
        }

        match pipeline.process_opportunity(&opp).await {
            Ok(status) => {
                last_submission = Instant::now();
                tracing::debug!(
                    opp_id = %opp.id,
                    status = ?status,
                    expected_profit = opp.expected_profit_lamports,
                    high_priority = is_high_profit,
                    "Bundle submitted"
                );
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

    tracing::warn!("Bundle submission task ended (channel closed)");
}

// =============================================================================
// TURBINE UDP TASK (Primary - Ultra-Low-Latency)
// =============================================================================

/// Turbine UDP task - receives shreds directly from validator for ultra-low-latency TX detection
async fn run_turbine_task(
    pipeline: Arc<UnifiedMevPipeline>,
    turbine_config: TurbineUdpConfig,
    stats: Arc<UnifiedPipelineStats>,
) {
    tracing::info!(
        bind_addr = %turbine_config.bind_addr,
        threads = turbine_config.receiver_threads,
        buffer_mb = turbine_config.recv_buffer_size / (1024 * 1024),
        "Starting Turbine UDP receiver"
    );

    // Convert config to TurbineConfig
    let bind_addr: SocketAddr = match turbine_config.bind_addr.parse() {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!("Invalid Turbine bind address '{}': {}", turbine_config.bind_addr, e);
            return;
        }
    };

    let config = TurbineConfig {
        bind_addr,
        validator_tvu_addr: None,
        recv_buffer_size: turbine_config.recv_buffer_size,
        num_receiver_threads: turbine_config.receiver_threads,
        fec_config: FecAssemblerConfig {
            max_tracked_fec_sets: turbine_config.max_fec_sets,
            fec_set_ttl: Duration::from_millis(turbine_config.fec_timeout_ms),
            ..Default::default()
        },
        min_slot: None,
        enable_dedup: true,
        shred_channel_size: turbine_config.shred_channel_size,
        fec_channel_size: 1000,
    };

    let mut receiver = TurbineReceiver::new(config);

    loop {
        match run_turbine_loop(&pipeline, &mut receiver, &stats).await {
            Ok(()) => {
                tracing::info!("Turbine task ended normally");
                break;
            }
            Err(e) => {
                tracing::warn!("Turbine error: {}, restarting in 3s...", e);
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

async fn run_turbine_loop(
    pipeline: &Arc<UnifiedMevPipeline>,
    receiver: &mut TurbineReceiver,
    stats: &Arc<UnifiedPipelineStats>,
) -> Result<()> {
    // Start receiver and get FEC completion channel
    let fec_rx = receiver.start().await?;
    
    // Create channel for assembled entries
    let (entries_tx, mut entries_rx) = mpsc::channel::<AssembledEntries>(1000);
    
    // Spawn entry assembler task
    tokio::spawn(async move {
        run_entry_assembler(fec_rx, entries_tx).await;
    });
    
    tracing::info!("Turbine UDP receiver started - waiting for shreds from validator TVU");

    // Process assembled entries
    while let Some(assembled) = entries_rx.recv().await {
        let slot = assembled.slot;
        
        // Update Turbine stats
        stats.turbine_fec_completed.fetch_add(1, Ordering::Relaxed);
        stats.turbine_entries_assembled.fetch_add(assembled.entries.len() as u64, Ordering::Relaxed);
        
        // Also update receiver stats
        let turbine_stats = receiver.stats();
        stats.turbine_packets_received.store(turbine_stats.packets_received, Ordering::Relaxed);
        stats.turbine_data_shreds.store(turbine_stats.data_shreds, Ordering::Relaxed);
        stats.turbine_coding_shreds.store(turbine_stats.coding_shreds, Ordering::Relaxed);
        
        // Convert AssembledEntries to Vec<SolanaEntry> for pipeline processing
        // The entries are already SolanaEntry type from the assembler
        let entries = assembled.entries;
        
        if entries.is_empty() {
            continue;
        }
        
        // Handle entries - this adds pending swaps to predictive state
        let all_swaps = pipeline.handle_shredstream_entry(slot, entries).await;
        
        // Track source
        stats.tx_source_turbine.fetch_add(assembled.transactions.len() as u64, Ordering::Relaxed);

        // Evaluate opportunities for each TX with swaps
        for tx_swaps in &all_swaps {
            let opportunities = pipeline.evaluate_opportunities(tx_swaps).await;

            // Send opportunities to submission task (non-blocking!)
            for opp in opportunities {
                pipeline.submit_opportunity(opp).await;
            }
        }
    }

    Ok(())
}

// =============================================================================
// SHREDSTREAM TASK (Fallback)
// =============================================================================

/// ShredStream task - receives entries and detects swaps EARLY
async fn run_shredstream_task(pipeline: Arc<UnifiedMevPipeline>, config: UnifiedPipelineConfig) {
    loop {
        match run_shredstream_loop(&pipeline, &config).await {
            Ok(()) => {
                tracing::info!("ShredStream task ended normally");
                break;
            }
            Err(e) => {
                tracing::warn!("ShredStream error: {}, reconnecting in 3s...", e);
                pipeline
                    .stats
                    .shredstream_reconnects
                    .fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

async fn run_shredstream_loop(
    pipeline: &Arc<UnifiedMevPipeline>,
    config: &UnifiedPipelineConfig,
) -> Result<()> {
    tracing::info!("ShredStream connecting to {}", config.shredstream_endpoint);

    let channel = Channel::from_shared(config.shredstream_endpoint.clone())
        .context("Invalid shredstream endpoint")?
        .connect_timeout(config.connect_timeout)
        .timeout(config.request_timeout)
        .connect()
        .await
        .context("Failed to connect to shredstream proxy")?;

    let mut client = ShredstreamProxyClient::new(channel);

    let request = tonic::Request::new(SubscribeEntriesRequest {});
    let mut stream = client
        .subscribe_entries(request)
        .await
        .context("Failed to subscribe to shredstream entries")?
        .into_inner();

    tracing::info!("ShredStream subscription established");

    let bincode_opts = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .allow_trailing_bytes();

    while let Some(entry_msg) = stream.message().await? {
        let slot = entry_msg.slot;
        pipeline
            .stats
            .shredstream_entries_received
            .fetch_add(1, Ordering::Relaxed);

        // Deserialize entries
        let entries: Vec<SolanaEntry> = match bincode_opts.deserialize(&entry_msg.entries) {
            Ok(e) => e,
            Err(e) => {
                tracing::trace!("Failed to deserialize entries: {}", e);
                continue;
            }
        };

        // Handle entries - this adds pending swaps to predictive state
        let all_swaps = pipeline.handle_shredstream_entry(slot, entries).await;

        // Evaluate opportunities for each TX with swaps
        for tx_swaps in &all_swaps {
            let opportunities = pipeline.evaluate_opportunities(tx_swaps).await;

            // Send opportunities to submission task (non-blocking!)
            // Rate limiting is handled by the submission task, not here.
            for opp in opportunities {
                pipeline.submit_opportunity(opp).await;
            }
        }
    }

    Ok(())
}

/// Geyser tasks - provides ground truth for state confirmation
async fn run_geyser_tasks(pipeline: Arc<UnifiedMevPipeline>, config: UnifiedPipelineConfig) {
    loop {
        match run_geyser_loop(&pipeline, &config).await {
            Ok(()) => {
                tracing::info!("Geyser tasks ended normally");
                break;
            }
            Err(e) => {
                tracing::warn!("Geyser error: {}, reconnecting in 3s...", e);
                pipeline
                    .stats
                    .geyser_reconnects
                    .fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

async fn run_geyser_loop(
    pipeline: &Arc<UnifiedMevPipeline>,
    config: &UnifiedPipelineConfig,
) -> Result<()> {
    let geyser_config = GeyserConfig {
        endpoint: config.geyser_endpoint.clone(),
        connect_timeout: config.connect_timeout,
        request_timeout: config.request_timeout,
    };

    let client = GeyserClient::new(geyser_config).await?;

    // Subscribe to transactions (for confirming pending swaps)
    let mut tx_stream = client.subscribe_transactions().await?;

    // Subscribe to blocks (for blockhash updates)
    let mut block_stream = client.subscribe_blocks().await?;

    // Subscribe to slots (for timing)
    let mut slot_stream = client.subscribe_slots().await?;

    // Subscribe to vault accounts (for reserve updates) using PSM's vault_index
    let vault_addresses: Vec<Pubkey> = pipeline.predictive_state.all_vaults();

    let has_vault_subscription = !vault_addresses.is_empty();
    let mut account_stream = if has_vault_subscription {
        tracing::info!(
            "Subscribing to {} vault accounts for real-time reserve updates",
            vault_addresses.len()
        );
        Some(client.subscribe_accounts(vault_addresses).await?)
    } else {
        None
    };

    // Subscribe to DEX program accounts (for pool state/CLMM tick updates)
    // This replaces the separate spawn_pool_state_updater function
    let dex_programs = vec![
        *ORCA_WHIRLPOOL,
        *RAYDIUM_CLMM,
        *METEORA_DLMM,
        *METEORA_DAMM,
        *RAYDIUM_AMM,
        *PANCAKESWAP_CLMM,
    ];
    let mut program_stream = client.subscribe_programs(dex_programs).await?;

    tracing::info!(
        "Geyser subscriptions established (vaults={}, dex_programs=6)",
        has_vault_subscription
    );

    // Process all streams concurrently
    loop {
        tokio::select! {
            // Transaction updates - confirm pending swaps
            tx_result = tx_stream.message() => {
                match tx_result {
                    Ok(Some(update)) => {
                        if let Some(tx_update) = update.transaction {
                            // Skip votes
                            if config.skip_votes && tx_update.is_vote {
                                pipeline.stats.geyser_votes_skipped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }

                            // Parse signature
                            if let Ok(sig) = tx_update.signature.parse::<Signature>() {
                                // Determine success from meta
                                let success = tx_update.tx
                                    .as_ref()
                                    .and_then(|t| t.meta.as_ref())
                                    .map(|m| m.err.is_none())
                                    .unwrap_or(false);

                                // Use lazy fallback - only extract pools if signature not in index
                                let tx_for_fallback = tx_update.tx.clone();
                                pipeline.handle_geyser_transaction_lazy(
                                    sig,
                                    tx_update.slot,
                                    success,
                                    || extract_pools_from_geyser_tx(&tx_for_fallback),
                                );
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return Err(e.into()),
                }
            }

            // Block updates - blockhash
            block_result = block_stream.message() => {
                match block_result {
                    Ok(Some(update)) => {
                        if let Some(block) = update.block_update {
                            if let Ok(hash) = block.blockhash.parse::<Hash>() {
                                pipeline.update_blockhash(hash, block.slot).await;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return Err(e.into()),
                }
            }

            // Slot updates - timing
            slot_result = slot_stream.message() => {
                match slot_result {
                    Ok(Some(update)) => {
                        if let Some(slot_update) = update.slot_update {
                            let status = SlotStatus::from(slot_update.status);
                            pipeline.handle_geyser_slot_update(slot_update.slot, status);
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return Err(e.into()),
                }
            }

            // Account updates - vault balances for reserve tracking
            account_result = async {
                match &mut account_stream {
                    Some(stream) => {
                        let result: Result<Option<crate::geyser::proto::geyser::TimestampedAccountUpdate>, tonic::Status> = stream.message().await;
                        result
                    }
                    None => std::future::pending::<Result<Option<crate::geyser::proto::geyser::TimestampedAccountUpdate>, tonic::Status>>().await,
                }
            } => {
                match account_result {
                    Ok(Some(update)) => {
                        if let Some(account_update) = update.account_update {
                            // Parse vault address
                            if account_update.pubkey.len() == 32 {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&account_update.pubkey);
                                let vault_address = Pubkey::new_from_array(arr);

                                // Parse token account balance (SPL token accounts have balance at offset 64)
                                // Account data format: [mint(32), owner(32), amount(8), ...]
                                if account_update.data.len() >= 72 {
                                    let balance = u64::from_le_bytes(
                                        account_update.data[64..72].try_into().unwrap_or_default()
                                    );
                                    pipeline.handle_geyser_vault_update(
                                        &vault_address,
                                        balance,
                                        account_update.slot,
                                    );
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::warn!("Account stream ended");
                        account_stream = None;
                    }
                    Err(e) => {
                        tracing::warn!("Account stream error: {}", e);
                        account_stream = None;
                    }
                }
            }

            // DEX program account updates - pool state (CLMM tick, sqrt_price, etc.)
            program_result = program_stream.message() => {
                match program_result {
                    Ok(Some(update)) => {
                        if let Some(account_update) = update.account_update {
                            // Parse pubkey
                            if account_update.pubkey.len() == 32 {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&account_update.pubkey);
                                let address = Pubkey::new_from_array(arr);

                                // Parse owner (program_id)
                                if account_update.owner.len() == 32 {
                                    let mut owner_arr = [0u8; 32];
                                    owner_arr.copy_from_slice(&account_update.owner);
                                    let program_id = Pubkey::new_from_array(owner_arr);

                                    pipeline.handle_geyser_pool_update(
                                        address,
                                        &program_id,
                                        &account_update.data,
                                        account_update.slot,
                                    );
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::warn!("Program stream ended");
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    Ok(())
}

/// Extract pool addresses from a Geyser transaction by finding swap instructions.
///
/// Uses the centralized pool_account_index from programs.rs to determine
/// where the pool address is located for each DEX type.
fn extract_pools_from_geyser_tx(
    tx: &Option<crate::geyser::proto::storage::confirmed_block::ConfirmedTransaction>,
) -> Vec<Pubkey> {
    use crate::shredstream::programs::{get_dex_type, is_swap_instruction, pool_account_index};

    let mut pools = Vec::new();

    let Some(confirmed_tx) = tx else {
        return pools;
    };
    let Some(transaction) = &confirmed_tx.transaction else {
        return pools;
    };
    let Some(message) = &transaction.message else {
        return pools;
    };

    // Build account keys list
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

    // Check each instruction for swap programs
    for ix in &message.instructions {
        let program_idx = ix.program_id_index as usize;
        if program_idx >= account_keys.len() {
            continue;
        }
        let program_id = &account_keys[program_idx];

        // Check if this is a swap instruction
        if !is_swap_instruction(program_id, &ix.data) {
            continue;
        }

        // Get DEX type and pool account index
        let Some(dex) = get_dex_type(program_id) else {
            continue;
        };
        let pool_idx = pool_account_index(dex);

        // Get the instruction's account at the pool index
        let pool = ix
            .accounts
            .get(pool_idx)
            .and_then(|&idx| account_keys.get(idx as usize).copied());

        if let Some(pool_addr) = pool {
            if !pools.contains(&pool_addr) {
                pools.push(pool_addr);
            }
        }
    }

    pools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_pipeline_stats_summary() {
        let stats = UnifiedPipelineStats::default();
        let summary = stats.summary();
        assert!(summary.contains("SHRED"));
        assert!(summary.contains("GEYSER"));
        assert!(summary.contains("PENDING"));
        assert!(summary.contains("MEV"));
    }
}
