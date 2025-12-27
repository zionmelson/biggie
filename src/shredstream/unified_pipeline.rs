//! Cross-Pool Arbitrage Pipeline
//!
//! This pipeline finds and executes cross-pool arbitrage opportunities by
//! tracking pool state from two sources:
//!
//! - **Turbine UDP** (~0.5-2ms): Parse swaps from shreds, simulate reserve changes
//! - **Geyser** (~5-15ms): Authoritative vault balance updates
//!
//! # CRITICAL: Shreds Are Post-Execution
//!
//! Transactions in shreds have ALREADY BEEN EXECUTED. We cannot "arb_bundle" them
//! in the same block. Our strategy is:
//!
//! 1. See swap executed in Block N (via Turbine/Shreds)
//! 2. Update our state to reflect the new reserves
//! 3. Detect price divergence across pools for that pair
//! 4. Submit arb TX for Block N+1
//!
//! # Data Flow
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                    CROSS-POOL ARBITRAGE PIPELINE                        │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                         │
//! │  TURBINE (fast path)                 GEYSER (reliable path)             │
//! │  ═══════════════════                 ═══════════════════                │
//! │  • Receive shreds via UDP            • Subscribe vault accounts         │
//! │  • Parse executed swaps              • Get authoritative balances       │
//! │  • Simulate reserve change           • Update state directly            │
//! │         │                                   │                           │
//! │         └─────────────┬─────────────────────┘                           │
//! │                       │                                                 │
//! │                       ▼ ON ANY STATE CHANGE                             │
//! │  ┌─────────────────────────────────────────────────────────────────┐   │
//! │  │                    POOL STATE MANAGER                            │   │
//! │  │  • Single source of truth for all pool reserves                 │   │
//! │  │  • Pair index: (base, quote) → [pool1, pool2, ...]              │   │
//! │  │  • Update triggers arb evaluation                               │   │
//! │  └─────────────────────────────────────────────────────────────────┘   │
//! │                       │                                                 │
//! │                       ▼                                                 │
//! │  ┌─────────────────────────────────────────────────────────────────┐   │
//! │  │                    ARB EVALUATOR                                 │   │
//! │  │  • For each state update, check pair for divergence             │   │
//! │  │  • Compare prices across all pools for that pair                │   │
//! │  │  • Calculate optimal size, expected profit                      │   │
//! │  └─────────────────────────────────────────────────────────────────┘   │
//! │                       │                                                 │
//! │                       ▼ IF PROFITABLE                                   │
//! │  ┌─────────────────────────────────────────────────────────────────┐   │
//! │  │                    JITO SUBMISSION                               │   │
//! │  │  • Single-TX bundle (standalone arb!)                           │   │
//! │  │  • Flash loan → buy cheap → sell expensive → repay              │   │
//! │  └─────────────────────────────────────────────────────────────────┘   │
//! │                                                                         │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```

use crate::amm_math::CurveType;
use crate::geyser::{GeyserClient, GeyserConfig, SlotStatus};
use crate::jito::{BundleStatus, JitoClient};
use crate::pool_discovery::parse_pool_account;
use crate::pool_state::{PoolState, PoolStateManager};
use crate::shredstream::arb_bundle::{ArbBundleBuilder, BundleBuilderConfig, PureArbBundle};
use crate::shredstream::evaluator::{ArbEvaluator, ArbOpportunity, PureArbOpportunity};
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

    // === Pool state stats ===
    pub pending_swaps_added: AtomicU64,
    pub pending_swaps_confirmed: AtomicU64,
    pub pending_swaps_expired: AtomicU64,

    // === Arb stats ===
    pub opportunities_evaluated: AtomicU64,
    pub opportunities_found: AtomicU64,
    pub three_hop_opportunities: AtomicU64,
    pub pure_arb_opportunities: AtomicU64,
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
            "{} GEYSER[txs={} accts={}] PENDING[add={} conf={} exp={} cur={}] ARB[opps={} submitted={} accepted={}] profit={:.6} SOL",
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
// LEADER SCHEDULE CACHE
// =============================================================================

/// Cached leader schedule info to avoid per-opportunity RPC calls
/// Refresh every ~400ms (1 slot) instead of per-opportunity
struct LeaderScheduleCache {
    /// The slot the next Jito leader starts
    next_leader_slot: u64,
    /// The current slot when we fetched
    fetched_at_slot: u64,
    /// When we last refreshed
    refreshed_at: Instant,
}

impl Default for LeaderScheduleCache {
    fn default() -> Self {
        Self {
            next_leader_slot: 0,
            fetched_at_slot: 0,
            refreshed_at: Instant::now() - Duration::from_secs(60), // Force immediate refresh
        }
    }
}

impl LeaderScheduleCache {
    /// Check if cache is stale (older than 400ms / 1 slot)
    fn is_stale(&self) -> bool {
        self.refreshed_at.elapsed() > Duration::from_millis(400)
    }
    
    /// Get slots until next Jito leader, accounting for time passed since fetch
    fn slots_until_leader(&self, current_slot: u64) -> u64 {
        self.next_leader_slot.saturating_sub(current_slot)
    }
}

// =============================================================================
// UNIFIED PIPELINE
// =============================================================================

/// Channel for sending opportunities to the bundle submission task
pub type OpportunityTx = mpsc::Sender<ArbOpportunity>;
pub type OpportunityRx = mpsc::Receiver<ArbOpportunity>;

/// The unified arbitrage pipeline with tightly coupled Turbine + Geyser
pub struct UnifiedArbPipeline {
    config: UnifiedPipelineConfig,

    // === Shared State (the coupling point) ===
    /// Pool state manager - THE SINGLE SOURCE OF TRUTH for pool state
    /// Also handles vault→pool mapping internally via vault_index
    pool_state: Arc<PoolStateManager>,

    // === Components ===
    evaluator: RwLock<ArbEvaluator>,
    bundle_builder: RwLock<ArbBundleBuilder>,
    jito_client: Arc<JitoClient>,
    blockhash: Arc<RwLock<(Hash, u64)>>,

    // === Submission Channel ===
    /// Channel to send opportunities for async submission (non-blocking)
    opportunity_tx: OpportunityTx,

    // === Leader Schedule Cache ===
    /// Cached leader schedule to avoid per-opportunity RPC calls
    leader_cache: RwLock<LeaderScheduleCache>,

    // === Statistics ===
    stats: Arc<UnifiedPipelineStats>,
}

impl UnifiedArbPipeline {
    pub fn new(
        config: UnifiedPipelineConfig,
        tip_floor_cache: Option<Arc<TipFloorCache>>,
        payer: Arc<Keypair>,
        flash_loan_reserves: HashMap<Pubkey, crate::executor::FlashLoanReserve>,
        lookup_tables: Vec<solana_message::AddressLookupTableAccount>,
        jito_client: Arc<JitoClient>,
        opportunity_tx: OpportunityTx,
    ) -> Self {
        // Create a new PSM (no pre-loaded pools)
        Self::new_with_psm(
            config,
            tip_floor_cache,
            payer,
            flash_loan_reserves,
            lookup_tables,
            jito_client,
            opportunity_tx,
            None,
        )
    }

    /// Create pipeline with optional pre-initialized PoolStateManager
    /// 
    /// If `existing_psm` is Some, uses that PSM with pre-loaded pools.
    /// If None, creates a new empty PSM.
    pub fn new_with_psm(
        config: UnifiedPipelineConfig,
        tip_floor_cache: Option<Arc<TipFloorCache>>,
        payer: Arc<Keypair>,
        flash_loan_reserves: HashMap<Pubkey, crate::executor::FlashLoanReserve>,
        lookup_tables: Vec<solana_message::AddressLookupTableAccount>,
        jito_client: Arc<JitoClient>,
        opportunity_tx: OpportunityTx,
        existing_psm: Option<Arc<PoolStateManager>>,
    ) -> Self {
        let pool_state = existing_psm.unwrap_or_else(|| Arc::new(PoolStateManager::new()));

        // Derive anchor mints from configured flash loan reserves
        // This ensures we only try to flash loan tokens we have reserves for
        let anchor_mints: Vec<Pubkey> = flash_loan_reserves.keys().copied().collect();
        tracing::info!(
            "Derived {} anchor mints from flash loan reserves",
            anchor_mints.len()
        );

        // Create evaluator with PoolStateManager as the single source of truth
        let evaluator = ArbEvaluator::new(Arc::clone(&pool_state), anchor_mints);

        let mut bundle_builder = ArbBundleBuilder::new(
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
            pool_state,
            evaluator: RwLock::new(evaluator),
            bundle_builder: RwLock::new(bundle_builder),
            jito_client,
            blockhash: Arc::new(RwLock::new((Hash::default(), 0))),
            opportunity_tx,
            leader_cache: RwLock::new(LeaderScheduleCache::default()),
            stats: Arc::new(UnifiedPipelineStats::default()),
        }
    }

    /// Send an opportunity for async submission (non-blocking)
    pub async fn submit_opportunity(&self, opp: ArbOpportunity) {
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

    /// Get the pool state manager (for external integration)
    pub fn pool_state(&self) -> Arc<PoolStateManager> {
        Arc::clone(&self.pool_state)
    }

    /// Get pipeline statistics
    pub fn stats(&self) -> Arc<UnifiedPipelineStats> {
        Arc::clone(&self.stats)
    }

    /// Get pool state statistics as a formatted string
    pub fn pool_state_stats(&self) -> String {
        self.pool_state.stats_summary()
    }

    /// Get evaluator statistics as a formatted string
    pub async fn evaluator_stats(&self) -> String {
        let evaluator = self.evaluator.read().await;
        evaluator.stats_summary()
    }

    /// Get pair index stats (how many token pairs have multiple pools for arb)
    pub fn pair_index_stats(&self) -> String {
        self.pool_state.pair_index_stats()
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

    /// Handle a ShredStream entry - parse transactions and detect swaps
    /// 
    /// When we see a swap in shreds, we update pool state and trigger pure arb
    /// evaluation on the affected pool. This gives us ~5-15ms head start over
    /// waiting for Geyser.
    #[allow(deprecated)] // solana_entry::entry::Entry is marked for Agave Unstable API
    pub async fn handle_shredstream_entry(
        &self,
        slot: u64,
        entries: Vec<SolanaEntry>,
    ) -> Vec<TransactionSwaps> {
        let received_at = Instant::now();
        let mut all_swaps = Vec::new();
        let mut pools_to_scan: Vec<Pubkey> = Vec::new();

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

                    // Track pools affected by these swaps for pure arb evaluation
                    for swap in &tx_swaps.swaps {
                        self.pool_state
                            .on_shredstream_swap_detected(swap, slot, signature);
                        if !pools_to_scan.contains(&swap.pool) {
                            pools_to_scan.push(swap.pool);
                        }
                    }

                    all_swaps.push(tx_swaps);
                }
            }
        }
        
        // Trigger pure arb evaluation on affected pools
        if !pools_to_scan.is_empty() {
            self.evaluate_pure_arb_for_pools(&pools_to_scan).await;
        }

        all_swaps
    }
    
    /// Evaluate pure arb opportunities for a list of pools
    async fn evaluate_pure_arb_for_pools(&self, pools: &[Pubkey]) {
        // Get current blockhash
        let blockhash_data = self.blockhash.read().await;
        if blockhash_data.0 == Hash::default() {
            return;
        }
        let current_blockhash = blockhash_data.0;
        let current_slot = blockhash_data.1;
        drop(blockhash_data);
        
        // Check leader schedule once for all pools
        let (is_jito_leader, slots_until) = self.check_jito_leader(current_slot).await;
        if !is_jito_leader || slots_until > 10 {
            return;
        }
        
        for pool in pools {
            let pure_opp = {
                let evaluator = self.evaluator.read().await;
                evaluator.scan_pure_arb_for_pool(pool)
            };
            
            let Some(opportunity) = pure_opp else {
                continue;
            };
            
            self.stats.pure_arb_opportunities.fetch_add(1, Ordering::Relaxed);
            
            // Build and submit
            let bundle = {
                let builder = self.bundle_builder.read().await;
                match builder.build_pure_arb(&opportunity, current_blockhash) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::debug!(error = %e, "Failed to build pure arb bundle");
                        continue;
                    }
                }
            };
            
            tracing::info!(
                pool = %pool,
                profit = opportunity.expected_profit_lamports,
                tip = bundle.tip_lamports,
                "Submitting pure arb (shred-triggered)"
            );
            
            self.stats.bundles_submitted.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = self.jito_client.send_versioned_bundle(bundle.transactions()).await {
                tracing::warn!(error = %e, "Failed to submit pure arb bundle");
                self.stats.bundles_rejected.fetch_add(1, Ordering::Relaxed);
            }
        }
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
            .pool_state
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
            self.pool_state.record_signature_index_miss();
            for pool in &fallback_pools {
                self.pool_state
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
            self.pool_state.total_pending_swaps() as u64,
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
        self.pool_state
            .on_geyser_reserves_update(&pool, base_reserve, quote_reserve, slot);
    }

    /// Handle Geyser vault account update - updates pool reserves via PSM's vault_index
    ///
    /// This is the PRIMARY trigger for pure arb opportunities.
    /// When a vault balance changes, we immediately check if this creates
    /// a cross-pool price discrepancy we can arbitrage.
    pub async fn handle_geyser_vault_update_with_arb(&self, vault_address: &Pubkey, balance: u64, slot: u64) {
        // Count vault updates received
        self.stats
            .geyser_account_updates
            .fetch_add(1, Ordering::Relaxed);
        
        // Get the pool this vault belongs to (before updating)
        let pool_for_vault = self.pool_state.get_pool_for_vault(vault_address);
        
        // PSM handles the vault→pool lookup and reserve update internally
        let updated = self.pool_state
            .on_geyser_vault_address_update(vault_address, balance, slot);
        
        if !updated {
            return;
        }
        
        // Get current blockhash for transaction building
        let blockhash_data = self.blockhash.read().await;
        if blockhash_data.0 == Hash::default() {
            return; // No valid blockhash yet
        }
        let current_blockhash = blockhash_data.0;
        let current_slot = blockhash_data.1;
        drop(blockhash_data);
        
        // Trigger pure arb scan on the updated pool
        let Some((pool, _is_base)) = pool_for_vault else {
            return;
        };
        
        let pure_opp = {
            let evaluator = self.evaluator.read().await;
            evaluator.scan_pure_arb_for_pool(&pool)
        };
        
        let Some(opportunity) = pure_opp else {
            return;
        };
        
        self.stats.pure_arb_opportunities.fetch_add(1, Ordering::Relaxed);
        
        // Check if Jito leader is coming up (use cached schedule)
        let (is_jito_leader, slots_until) = self.check_jito_leader(current_slot).await;
        if !is_jito_leader || slots_until > 10 {
            tracing::debug!(
                pool = %pool,
                slots_until = slots_until,
                "Skipping pure arb - no Jito leader soon"
            );
            return;
        }
        
        // Build and submit pure arb bundle
        let bundle = {
            let builder = self.bundle_builder.read().await;
            match builder.build_pure_arb(&opportunity, current_blockhash) {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!(error = %e, "Failed to build pure arb bundle");
                    return;
                }
            }
        };
        
        tracing::info!(
            pool = %pool,
            profit = opportunity.expected_profit_lamports,
            tip = bundle.tip_lamports,
            buy_pool = %opportunity.buy_pool,
            sell_pool = %opportunity.sell_pool,
            "Submitting pure arb bundle"
        );
        
        // Submit to Jito
        self.stats.bundles_submitted.fetch_add(1, Ordering::Relaxed);
        match self.jito_client.send_versioned_bundle(bundle.transactions()).await {
            Ok(uuid) => {
                tracing::info!(bundle_uuid = %uuid, "Pure arb bundle submitted");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to submit pure arb bundle");
                self.stats.bundles_rejected.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Handle Geyser vault account update - updates pool reserves (sync version, no arb scan)
    pub fn handle_geyser_vault_update(&self, vault_address: &Pubkey, balance: u64, slot: u64) {
        // Count vault updates received (reuse geyser_account_updates counter)
        self.stats
            .geyser_account_updates
            .fetch_add(1, Ordering::Relaxed);
        // PSM handles the vault→pool lookup and reserve update internally
        let updated = self.pool_state
            .on_geyser_vault_address_update(vault_address, balance, slot);
        if updated {
            tracing::trace!(
                vault = %vault_address,
                balance = balance,
                slot = slot,
                "Vault reserve updated"
            );
        }
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
            // The pool_state manager handles this via TTL
        }
    }

    /// Handle Geyser DEX program account update - updates pool state (CLMM tick, sqrt_price, etc.)
    ///
    /// This replaces the separate `run_pool_state_updater` task, consolidating all Geyser
    /// subscriptions into the unified pipeline's single connection.
    ///
    /// Handles three types of accounts:
    /// 1. Pool accounts → updates pool state in PoolStateManager
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
        let tick_cache = self.pool_state.tick_cache();
        if tick_cache.process_account_update(&address, data, slot) {
            return; // Was a registered tick array
        }

        let bin_cache = self.pool_state.bin_cache();
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
        let is_new_pool = !self.pool_state.has_pool(&pool.address);

        // Register or update pool state (single source of truth)
        if is_new_pool {
            // Create and register new pool in pool state with metadata
            let mut pool_state = PoolState::new(
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

            self.pool_state.register_pool(pool_state);
        } else {
            // Update metadata for existing pool
            self.pool_state.update_pool_metadata(
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
            self.pool_state.update_clmm_state(
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
    // ARB EVALUATION (Uses Pool State)
    // =========================================================================

    /// Evaluate swaps for arbitrage opportunities using PREDICTED state
    ///
    /// This is the key function that benefits from state prediction.
    /// When we detect a swap, the predicted state already includes its
    /// impact, allowing accurate profit calculation for cross-pool arbs.
    pub async fn evaluate_opportunities(&self, tx_swaps: &TransactionSwaps) -> Vec<ArbOpportunity> {
        // The evaluator uses PoolStateManager for accurate results.
        // The pool_state has pending swaps factored in.
        // Using read lock since evaluate() now uses atomic stats
        let evaluator = self.evaluator.read().await;

        // Evaluate (the evaluator uses predicted state via PoolStateManager)
        let opportunities = evaluator.evaluate(tx_swaps);

        self.stats
            .opportunities_evaluated
            .fetch_add(tx_swaps.swaps.len() as u64, Ordering::Relaxed);
        self.stats
            .opportunities_found
            .fetch_add(opportunities.len() as u64, Ordering::Relaxed);

        opportunities
    }

    /// Check if a Jito-connected validator is the upcoming leader
    /// Returns (is_jito_leader, slots_until_leader)
    /// Uses cached leader schedule - refreshes every ~400ms (1 slot) instead of per-opportunity
    async fn check_jito_leader(&self, current_slot: u64) -> (bool, u64) {
        // Check if cache is stale
        {
            let cache = self.leader_cache.read().await;
            if !cache.is_stale() {
                let slots_until = cache.slots_until_leader(current_slot);
                return (cache.next_leader_slot > 0, slots_until);
            }
        }
        
        // Cache is stale, refresh it
        match self.jito_client.get_next_scheduled_leader().await {
            Ok(resp) => {
                let slots_until = resp.next_leader_slot.saturating_sub(resp.current_slot);
                
                // Update cache
                {
                    let mut cache = self.leader_cache.write().await;
                    cache.next_leader_slot = resp.next_leader_slot;
                    cache.fetched_at_slot = resp.current_slot;
                    cache.refreshed_at = Instant::now();
                }
                
                // Jito leader is scheduled - worth submitting
                (resp.next_leader_slot > 0, slots_until)
            }
            Err(_) => {
                // If we can't check, assume it's worth submitting
                (true, 0)
            }
        }
    }

    /// Process a single opportunity - build and submit bundle
    pub async fn process_opportunity(&self, opp: &ArbOpportunity) -> Result<BundleStatus> {
        // Get current blockhash and slot
        let (blockhash, current_slot) = *self.blockhash.read().await;
        if blockhash == Hash::default() {
            anyhow::bail!("No blockhash available");
        }
        
        // Check if Jito validator is leader soon (uses cached schedule)
        // If no Jito leader for 10+ slots, skip to avoid wasted submission
        let (is_jito_leader, slots_until) = self.check_jito_leader(current_slot).await;
        if !is_jito_leader || slots_until > 10 {
            // Log but don't fail - just skip this opportunity
            tracing::debug!(
                opp_id = %opp.id,
                slots_until = slots_until,
                "Skipping submission - no Jito leader soon"
            );
            // Don't bail - return a status indicating we skipped
            return Ok(BundleStatus::Submitted { 
                bundle_id: format!("skipped_no_leader_{}", opp.id) 
            });
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
            slots_until_leader = slots_until,
            "Submitting bundle to Jito"
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

        // Log evaluator stats (critical for debugging why no opps are found)
        if let Ok(evaluator) = self.evaluator.try_read() {
            tracing::info!("EVALUATOR: {}", evaluator.stats_summary());
        }

        // Also log pool state stats
        let ps_stats = self.pool_state.stats();
        tracing::debug!(
            "POOL STATE: pools={} pending_total={} confirmed={} expired={}",
            ps_stats.pools_tracked,
            ps_stats.pending_swaps_total,
            ps_stats.pending_swaps_confirmed,
            ps_stats.pending_swaps_expired
        );

        // Log pair index stats for arb route debugging
        tracing::debug!("PAIR INDEX: {}", self.pool_state.pair_index_stats());
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
///
/// If `existing_psm` is provided, uses that pre-initialized PoolStateManager
/// with pre-loaded pools. This enables Geyser vault subscriptions from startup.
pub async fn spawn_unified_pipeline(
    config: UnifiedPipelineConfig,
    tip_floor_cache: Option<Arc<TipFloorCache>>,
    payer: Arc<Keypair>,
    flash_loan_reserves: HashMap<Pubkey, crate::executor::FlashLoanReserve>,
    lookup_tables: Vec<solana_message::AddressLookupTableAccount>,
    jito_client: Arc<JitoClient>,
    existing_psm: Option<Arc<PoolStateManager>>,
) -> Result<(Arc<UnifiedArbPipeline>, Arc<UnifiedPipelineStats>)> {
    // Log pre-loaded pool count if PSM was provided
    if let Some(ref psm) = existing_psm {
        let stats = psm.stats();
        let vault_count = psm.all_vaults().len();
        tracing::info!(
            "Using pre-initialized PSM: {} pools, {} vaults for Geyser subscription",
            stats.pools_tracked,
            vault_count
        );
    }

    // Create channel for async opportunity submission (bounded to prevent OOM)
    let (opportunity_tx, opportunity_rx) = mpsc::channel::<ArbOpportunity>(100);

    let pipeline = Arc::new(UnifiedArbPipeline::new_with_psm(
        config.clone(),
        tip_floor_cache,
        payer,
        flash_loan_reserves,
        lookup_tables,
        jito_client,
        opportunity_tx,
        existing_psm,
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

    // NOTE: Pure arb scanning is now triggered by Geyser vault updates (event-driven)
    // rather than polling. See handle_geyser_vault_update_with_arb().
    // The 50ms polling task has been removed to reduce CPU overhead.

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

/// Bundle submission task - consumes opportunities and submits IMMEDIATELY
///
/// This runs separately from entry processing to not block the ShredStream loop.
///
/// COMPETITIVE EDGE: NO RATE LIMITING. In arb, every millisecond counts.
/// Jito handles deduplication on their end. We submit as fast as possible.
async fn run_bundle_submission_task(
    pipeline: Arc<UnifiedArbPipeline>,
    mut opportunity_rx: OpportunityRx,
    _config: UnifiedPipelineConfig,
) {
    tracing::info!("Bundle submission task started (NO rate limiting - max speed)");

    while let Some(opp) = opportunity_rx.recv().await {
        // NO RATE LIMITING - submit immediately
        // Speed is everything in arb. Jito dedupes on their end.
        let start = Instant::now();
        
        match pipeline.process_opportunity(&opp).await {
            Ok(status) => {
                let submit_time_us = start.elapsed().as_micros();
                tracing::info!(
                    opp_id = %opp.id,
                    status = ?status,
                    profit_lamports = opp.expected_profit_lamports,
                    submit_time_us = submit_time_us,
                    "Bundle submitted"
                );
            }
            Err(e) => {
                tracing::warn!(
                    opp_id = %opp.id,
                    error = %e,
                    "Failed to submit bundle"
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
    pipeline: Arc<UnifiedArbPipeline>,
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
    pipeline: &Arc<UnifiedArbPipeline>,
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
        
        // Handle entries - this adds pending swaps to pool state
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
async fn run_shredstream_task(pipeline: Arc<UnifiedArbPipeline>, config: UnifiedPipelineConfig) {
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
    pipeline: &Arc<UnifiedArbPipeline>,
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

        // Handle entries - this adds pending swaps to pool state
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
async fn run_geyser_tasks(pipeline: Arc<UnifiedArbPipeline>, config: UnifiedPipelineConfig) {
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
    pipeline: &Arc<UnifiedArbPipeline>,
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
    let vault_addresses: Vec<Pubkey> = pipeline.pool_state.all_vaults();

    let has_vault_subscription = !vault_addresses.is_empty();
    let mut account_stream = if has_vault_subscription {
        tracing::info!(
            "Subscribing to {} vault accounts for real-time reserve updates (first 3: {:?})",
            vault_addresses.len(),
            vault_addresses.iter().take(3).map(|v| v.to_string()).collect::<Vec<_>>()
        );
        let stream = client.subscribe_accounts(vault_addresses).await?;
        tracing::info!("Account subscription stream created successfully");
        Some(stream)
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
                        tracing::trace!("Received account update from Geyser");
                        if let Some(account_update) = update.account_update {
                            // Parse vault address
                            if account_update.pubkey.len() == 32 {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&account_update.pubkey);
                                let vault_address = Pubkey::new_from_array(arr);
                                tracing::debug!(
                                    vault = %vault_address,
                                    data_len = account_update.data.len(),
                                    slot = account_update.slot,
                                    "Account update received"
                                );

                                // Parse token account balance (SPL token accounts have balance at offset 64)
                                // Account data format: [mint(32), owner(32), amount(8), ...]
                                if account_update.data.len() >= 72 {
                                    let balance = u64::from_le_bytes(
                                        account_update.data[64..72].try_into().unwrap_or_default()
                                    );
                                    tracing::debug!(
                                        vault = %vault_address,
                                        balance = balance,
                                        "Parsed vault balance, calling handle_geyser_vault_update"
                                    );
                                    pipeline.handle_geyser_vault_update(
                                        &vault_address,
                                        balance,
                                        account_update.slot,
                                    );
                                } else {
                                    tracing::warn!(
                                        vault = %vault_address,
                                        data_len = account_update.data.len(),
                                        "Account data too short for token account"
                                    );
                                }
                            } else {
                                tracing::warn!("Account update pubkey wrong length: {}", account_update.pubkey.len());
                            }
                        } else {
                            tracing::warn!("Account update missing account_update field");
                        }
                    }
                    Ok(None) => {
                        tracing::error!("Account stream ended unexpectedly - this is a critical issue!");
                        account_stream = None;
                    }
                    Err(e) => {
                        tracing::error!("Account stream error: {} - this is a critical issue!", e);
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

// NOTE: The 50ms polling task (run_pure_arb_scan_task) has been removed.
// Pure arb is now triggered by:
// 1. Geyser vault updates (handle_geyser_vault_update_with_arb)
// 2. Turbine/ShredStream swap detection (evaluate_pure_arb_for_pools)
// This is more efficient - we only evaluate when state actually changes.

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
        assert!(summary.contains("ARB"));
    }
}
