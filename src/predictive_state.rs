//! Unified Predictive State Manager
//!
//! Tightly integrates ShredStream (early TX detection) and Geyser (ground truth)
//! for accurate MEV opportunity evaluation with state prediction.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    PredictiveStateManager                       │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  ConfirmedState ◄── Geyser AccountUpdates (ground truth)        │
//! │       │                                                         │
//! │       ▼                                                         │
//! │  PendingSwaps ◄── ShredStream entries (early detection)         │
//! │       │                                                         │
//! │       ▼                                                         │
//! │  PredictedState = ConfirmedState + simulate(PendingSwaps)       │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Key Invariants
//!
//! 1. **ConfirmedState** is ONLY updated by Geyser (never ShredStream)
//! 2. **PendingSwaps** are added by ShredStream, removed by Geyser confirmation
//! 3. **PredictedState** is recomputed on every state change
//! 4. **MEV evaluation** ALWAYS uses PredictedState, never ConfirmedState directly
//!
//! # Timing Flow
//!
//! ```text
//! t=0ms   ShredStream: Victim TX detected → added to PendingSwaps
//! t=0ms   PredictedState recalculated (includes victim's impact)
//! t=1ms   MEV Evaluator uses PredictedState → finds opportunity
//! t=5ms   Geyser: Victim TX confirmed → PendingSwap removed
//! t=5ms   ConfirmedState updated → PredictedState recalculated
//! t=6ms   Future evaluations use fresh ground truth
//! ```

use crate::amm_math::{CurveType, PoolSimulator, VirtualPoolState};
use crate::bin_array_cache::BinArrayCache;
use crate::clmm_math::ClmmPoolState;
use crate::dlmm_math::DlmmPoolState;
use crate::shredstream::parser::{ParsedSwap, SwapDirection};
use crate::shredstream::programs::DexType;
use crate::tick_array_cache::{ClmmPoolType, TickArrayCache};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Maximum age of a pending swap before it's considered expired
const PENDING_SWAP_TTL: Duration = Duration::from_secs(5);

/// Maximum number of pending swaps per pool (prevents memory bloat)
const MAX_PENDING_SWAPS_PER_POOL: usize = 50;

/// How often to run expiry cleanup
const CLEANUP_INTERVAL: Duration = Duration::from_secs(1);

// =============================================================================
// PENDING SWAP TRACKING
// =============================================================================

/// A swap detected via ShredStream but not yet confirmed by Geyser
#[derive(Clone, Debug)]
pub struct PendingSwap {
    /// Transaction signature (for matching with Geyser confirmation)
    pub signature: Signature,
    /// Slot where the swap was detected
    pub slot: u64,
    /// Pool address
    pub pool: Pubkey,
    /// DEX type (for simulation math)
    pub dex: DexType,
    /// Input amount
    pub amount_in: u64,
    /// Direction: true = base→quote, false = quote→base
    pub base_to_quote: bool,
    /// When we detected this swap (for TTL)
    pub detected_at: Instant,
    /// Simulated output (cached)
    pub simulated_output: Option<u64>,
    /// Status
    pub status: PendingSwapStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingSwapStatus {
    /// Detected via ShredStream, awaiting Geyser confirmation
    Pending,
    /// Confirmed by Geyser TransactionUpdate
    Confirmed,
    /// Expired (exceeded TTL without confirmation)
    Expired,
    /// Failed (Geyser reported transaction failure)
    Failed,
}

// =============================================================================
// POOL PREDICTIVE STATE
// =============================================================================

/// Complete state for a single pool with prediction capability
#[derive(Clone, Debug)]
pub struct PoolPredictiveState {
    /// Pool address
    pub address: Pubkey,
    /// DEX type
    pub dex: DexType,

    // === CONFIRMED STATE (from Geyser) ===
    /// Base token mint
    pub base_mint: Pubkey,
    /// Quote token mint  
    pub quote_mint: Pubkey,
    /// Confirmed base reserves (from Geyser AccountUpdate)
    pub confirmed_base_reserve: u64,
    /// Confirmed quote reserves (from Geyser AccountUpdate)
    pub confirmed_quote_reserve: u64,
    /// Decimals for proper amount handling
    pub base_decimals: u8,
    pub quote_decimals: u8,
    /// Fee in basis points
    pub fee_bps: u16,
    /// Curve type
    pub curve: CurveType,
    /// Last slot where Geyser confirmed state
    pub last_confirmed_slot: u64,
    /// When confirmed state was last updated
    pub last_confirmed_at: Instant,

    // === CLMM-SPECIFIC CONFIRMED STATE ===
    /// sqrt_price_x64 for tick-based CLMM
    pub confirmed_sqrt_price: Option<u128>,
    /// Active tick (CLMM) or bin_id (DLMM)
    pub confirmed_tick_or_bin: Option<i32>,
    /// Tick spacing / bin step
    pub spacing: Option<u16>,
    /// Active liquidity in current range
    pub active_liquidity: Option<u128>,

    // === POOL METADATA (for swap instruction building) ===
    /// Token A vault
    pub token_vault_a: Pubkey,
    /// Token B vault
    pub token_vault_b: Pubkey,
    /// AMM authority PDA (Raydium AMM)
    pub amm_authority: Option<Pubkey>,
    /// AMM config account (Raydium CLMM, PancakeSwap CLMM)
    pub amm_config: Option<Pubkey>,
    /// Observation/Oracle state (CLMM pools)
    pub observation_state: Option<Pubkey>,
    /// Tick arrays for CLMM swaps
    pub tick_arrays: Vec<Pubkey>,
    /// Bin array bitmap extension (DLMM)
    pub bin_array_bitmap_extension: Option<Pubkey>,
    /// Bin arrays for DLMM swaps
    pub bin_arrays: Vec<Pubkey>,
    /// Pool authority PDA (DAMM)
    pub pool_authority: Option<Pubkey>,

    // === PENDING SWAPS (from ShredStream) ===
    /// Queue of pending swaps, ordered by detection time
    pending_swaps: VecDeque<PendingSwap>,

    // === PREDICTED STATE (computed) ===
    /// Predicted base reserves after applying pending swaps
    pub predicted_base_reserve: u64,
    /// Predicted quote reserves after applying pending swaps
    pub predicted_quote_reserve: u64,
    /// Predicted sqrt_price (for CLMM, approximated)
    pub predicted_sqrt_price: Option<u128>,
    /// Predicted tick/bin (for CLMM, approximated)
    pub predicted_tick_or_bin: Option<i32>,
    /// When prediction was last recalculated
    pub prediction_computed_at: Instant,
    /// Number of pending swaps included in prediction
    pub pending_swap_count: usize,
}

impl PoolPredictiveState {
    /// Create a new pool state (initially confirmed = predicted)
    pub fn new(
        address: Pubkey,
        dex: DexType,
        base_mint: Pubkey,
        quote_mint: Pubkey,
        base_reserve: u64,
        quote_reserve: u64,
        base_decimals: u8,
        quote_decimals: u8,
        fee_bps: u16,
        curve: CurveType,
        slot: u64,
    ) -> Self {
        let now = Instant::now();
        Self {
            address,
            dex,
            base_mint,
            quote_mint,
            confirmed_base_reserve: base_reserve,
            confirmed_quote_reserve: quote_reserve,
            base_decimals,
            quote_decimals,
            fee_bps,
            curve,
            last_confirmed_slot: slot,
            last_confirmed_at: now,
            confirmed_sqrt_price: None,
            confirmed_tick_or_bin: None,
            spacing: None,
            // Initialize metadata with defaults (will be set via set_metadata)
            token_vault_a: Pubkey::default(),
            token_vault_b: Pubkey::default(),
            amm_authority: None,
            amm_config: None,
            observation_state: None,
            tick_arrays: Vec::new(),
            bin_array_bitmap_extension: None,
            bin_arrays: Vec::new(),
            pool_authority: None,
            active_liquidity: None,
            pending_swaps: VecDeque::new(),
            predicted_base_reserve: base_reserve,
            predicted_quote_reserve: quote_reserve,
            predicted_sqrt_price: None,
            predicted_tick_or_bin: None,
            prediction_computed_at: now,
            pending_swap_count: 0,
        }
    }

    /// Set pool metadata for swap instruction building
    pub fn set_metadata(
        &mut self,
        token_vault_a: Pubkey,
        token_vault_b: Pubkey,
        amm_authority: Option<Pubkey>,
        amm_config: Option<Pubkey>,
        observation_state: Option<Pubkey>,
        tick_arrays: Vec<Pubkey>,
        bin_array_bitmap_extension: Option<Pubkey>,
        bin_arrays: Vec<Pubkey>,
        pool_authority: Option<Pubkey>,
    ) {
        self.token_vault_a = token_vault_a;
        self.token_vault_b = token_vault_b;
        self.amm_authority = amm_authority;
        self.amm_config = amm_config;
        self.observation_state = observation_state;
        self.tick_arrays = tick_arrays;
        self.bin_array_bitmap_extension = bin_array_bitmap_extension;
        self.bin_arrays = bin_arrays;
        self.pool_authority = pool_authority;
    }

    /// Check if pool has valid metadata (non-default vaults)
    pub fn has_metadata(&self) -> bool {
        self.token_vault_a != Pubkey::default() && self.token_vault_b != Pubkey::default()
    }

    // =========================================================================
    // GEYSER UPDATES (Ground Truth)
    // =========================================================================

    /// Update confirmed state from Geyser AccountUpdate
    /// This is the AUTHORITATIVE state update
    pub fn update_confirmed_reserves(&mut self, base_reserve: u64, quote_reserve: u64, slot: u64) {
        // Only update if this is newer than what we have
        if slot < self.last_confirmed_slot {
            return;
        }

        self.confirmed_base_reserve = base_reserve;
        self.confirmed_quote_reserve = quote_reserve;
        self.last_confirmed_slot = slot;
        self.last_confirmed_at = Instant::now();

        // Recalculate prediction
        self.recalculate_prediction();
    }

    /// Update CLMM state from Geyser AccountUpdate
    pub fn update_confirmed_clmm(
        &mut self,
        sqrt_price: Option<u128>,
        tick_or_bin: i32,
        spacing: u16,
        liquidity: Option<u128>,
        slot: u64,
    ) {
        if slot < self.last_confirmed_slot {
            return;
        }

        self.confirmed_sqrt_price = sqrt_price;
        self.confirmed_tick_or_bin = Some(tick_or_bin);
        self.spacing = Some(spacing);
        self.active_liquidity = liquidity;
        self.last_confirmed_slot = slot;
        self.last_confirmed_at = Instant::now();

        self.recalculate_prediction();
    }

    /// Mark a pending swap as confirmed (Geyser saw it execute)
    pub fn confirm_pending_swap(&mut self, signature: &Signature) {
        if let Some(swap) = self
            .pending_swaps
            .iter_mut()
            .find(|s| s.signature == *signature)
        {
            swap.status = PendingSwapStatus::Confirmed;
        }
        // Remove confirmed swaps and recalculate
        self.pending_swaps
            .retain(|s| s.status == PendingSwapStatus::Pending);
        self.recalculate_prediction();
    }

    /// Mark a pending swap as failed (Geyser saw it fail)
    pub fn fail_pending_swap(&mut self, signature: &Signature) {
        self.pending_swaps.retain(|s| s.signature != *signature);
        self.recalculate_prediction();
    }

    // =========================================================================
    // SHREDSTREAM UPDATES (Early Detection)
    // =========================================================================

    /// Add a pending swap detected via ShredStream
    /// Returns true if the swap was added (not a duplicate)
    pub fn add_pending_swap(&mut self, swap: PendingSwap) -> bool {
        // Check for duplicate
        if self
            .pending_swaps
            .iter()
            .any(|s| s.signature == swap.signature)
        {
            return false;
        }

        // Enforce max pending swaps
        while self.pending_swaps.len() >= MAX_PENDING_SWAPS_PER_POOL {
            self.pending_swaps.pop_front();
        }

        self.pending_swaps.push_back(swap);
        self.recalculate_prediction();
        true
    }

    /// Expire old pending swaps
    pub fn expire_old_swaps(&mut self) -> usize {
        let now = Instant::now();
        let before = self.pending_swaps.len();

        self.pending_swaps.retain(|s| {
            s.status == PendingSwapStatus::Pending
                && now.duration_since(s.detected_at) < PENDING_SWAP_TTL
        });

        let expired = before - self.pending_swaps.len();
        if expired > 0 {
            self.recalculate_prediction();
        }
        expired
    }

    // =========================================================================
    // STATE PREDICTION
    // =========================================================================

    /// Recalculate predicted state by simulating all pending swaps on confirmed state.
    ///
    /// Uses curve-specific math:
    /// - ConstantProduct: VirtualPoolState (x * y = k)
    /// - ConcentratedLiquidity: ClmmPoolState (tick-based, from Raydium CLMM)
    /// - Dlmm: Uses constant product approximation (full bin simulation requires bin data)
    fn recalculate_prediction(&mut self) {
        // Collect pending swaps data (clone to avoid borrow issues)
        let pending: Vec<(u64, bool)> = self
            .pending_swaps
            .iter()
            .filter(|s| s.status == PendingSwapStatus::Pending)
            .map(|s| (s.amount_in, s.base_to_quote))
            .collect();

        if pending.is_empty() {
            // No pending swaps, predicted = confirmed
            self.predicted_base_reserve = self.confirmed_base_reserve;
            self.predicted_quote_reserve = self.confirmed_quote_reserve;
            self.predicted_sqrt_price = self.confirmed_sqrt_price;
            self.predicted_tick_or_bin = self.confirmed_tick_or_bin;
            self.pending_swap_count = 0;
            self.prediction_computed_at = Instant::now();
            return;
        }

        match self.curve {
            CurveType::ConstantProduct | CurveType::Dlmm => {
                // DLMM uses constant product approximation for pending swap prediction
                // Full bin-based simulation requires cached bin data
                self.recalculate_constant_product(&pending);
            }
            CurveType::ConcentratedLiquidity => {
                self.recalculate_clmm(&pending);
            }
        }

        self.pending_swap_count = pending.len();
        self.prediction_computed_at = Instant::now();

        tracing::trace!(
            pool = %self.address,
            curve = ?self.curve,
            pending = self.pending_swap_count,
            confirmed = format!("({}, {})", self.confirmed_base_reserve, self.confirmed_quote_reserve),
            predicted = format!("({}, {})", self.predicted_base_reserve, self.predicted_quote_reserve),
            "State prediction updated"
        );
    }

    /// Recalculate using constant product formula (x * y = k).
    /// Reference: Raydium AMM Calculator::swap_token_amount_base_in
    fn recalculate_constant_product(&mut self, pending: &[(u64, bool)]) {
        let mut virtual_state = VirtualPoolState::new(
            self.confirmed_base_reserve,
            self.confirmed_quote_reserve,
            self.fee_bps,
        );

        for &(amount_in, base_to_quote) in pending {
            let (_, new_state) = virtual_state.simulate_swap(amount_in, base_to_quote);
            virtual_state = new_state;
        }

        self.predicted_base_reserve = virtual_state.base_reserve;
        self.predicted_quote_reserve = virtual_state.quote_reserve;
    }

    /// Recalculate using CLMM tick-based math.
    /// Reference: raydium-io/raydium-clmm swap.rs::swap_internal
    fn recalculate_clmm(&mut self, pending: &[(u64, bool)]) {
        // Get CLMM parameters from confirmed state
        let sqrt_price = match self.confirmed_sqrt_price {
            Some(sp) => sp,
            None => {
                // Fallback to constant product if CLMM data unavailable
                tracing::warn!(
                    pool = %self.address,
                    "CLMM pool missing sqrt_price, falling back to constant product"
                );
                self.recalculate_constant_product(pending);
                return;
            }
        };

        let tick = self.confirmed_tick_or_bin.unwrap_or(0);
        let spacing = self.spacing.unwrap_or(1);
        let liquidity = self.active_liquidity.unwrap_or(0);

        if liquidity == 0 {
            // No liquidity, use constant product fallback
            tracing::warn!(
                pool = %self.address,
                "CLMM pool has zero liquidity, falling back to constant product"
            );
            self.recalculate_constant_product(pending);
            return;
        }

        // Convert fee_bps to fee_rate in millionths (e.g., 30 bps = 3000)
        let fee_rate = (self.fee_bps as u32) * 100;

        let mut clmm_state = ClmmPoolState::new(sqrt_price, tick, spacing, liquidity, fee_rate);

        // NOTE: In production, you'd populate ticks from on-chain tick array data
        // For now, we simulate with current liquidity (single-range approximation)
        // This is still more accurate than the old sqrt(ratio) approximation

        for &(amount_in, base_to_quote) in pending {
            // zero_for_one: selling base (token0) for quote (token1) = price decreases
            let zero_for_one = base_to_quote;

            let (_amount_in, _amount_out, new_state) = clmm_state.simulate_swap(
                amount_in,
                zero_for_one,
                true, // is_base_input (exact input)
                0,    // no price limit
            );

            if _amount_in > 0 {
                clmm_state = new_state;
            }
        }

        // Update predicted CLMM state
        self.predicted_sqrt_price = Some(clmm_state.sqrt_price_x64);

        // Estimate reserves from sqrt_price and liquidity using CLMM formulas:
        // base_reserve ≈ L / sqrt(P)
        // quote_reserve ≈ L * sqrt(P)
        // These are approximations for the current tick range
        let sqrt_p_float = (clmm_state.sqrt_price_x64 as f64) / ((1u128 << 64) as f64);
        if sqrt_p_float > 0.0 && clmm_state.liquidity > 0 {
            let l = clmm_state.liquidity as f64;
            self.predicted_base_reserve = (l / sqrt_p_float) as u64;
            self.predicted_quote_reserve = (l * sqrt_p_float) as u64;
        }

        // Update tick from price
        self.predicted_tick_or_bin = Some(clmm_state.tick);
    }

    /// Get a VirtualPoolState for simulation using PREDICTED reserves
    pub fn predicted_virtual_state(&self) -> VirtualPoolState {
        VirtualPoolState::new(
            self.predicted_base_reserve,
            self.predicted_quote_reserve,
            self.fee_bps,
        )
    }

    /// Get a PoolSimulator using PREDICTED state with curve-specific math.
    ///
    /// This is the preferred method for accurate simulation:
    /// - ConstantProduct pools → VirtualPoolState
    /// - ConcentratedLiquidity pools → ClmmPoolState  
    /// - DLMM pools → DlmmPoolState (via DEX type detection)
    pub fn predicted_simulator(&self) -> PoolSimulator {
        match self.curve {
            CurveType::ConcentratedLiquidity => {
                // Build CLMM state from predicted values
                if let (Some(sqrt_price), Some(tick), Some(spacing), Some(liquidity)) = (
                    self.predicted_sqrt_price.or(self.confirmed_sqrt_price),
                    self.predicted_tick_or_bin.or(self.confirmed_tick_or_bin),
                    self.spacing,
                    self.active_liquidity,
                ) {
                    let fee_rate = (self.fee_bps as u32) * 100; // bps to millionths
                    let clmm = ClmmPoolState::new(sqrt_price, tick, spacing, liquidity, fee_rate);
                    return PoolSimulator::Clmm(clmm);
                }
                // Fallback to constant product if CLMM data incomplete
                tracing::warn!(
                    pool = %self.address,
                    "CLMM pool missing data, using constant product fallback"
                );
                PoolSimulator::ConstantProduct(self.predicted_virtual_state())
            }
            CurveType::Dlmm => {
                // Build DLMM state from predicted values
                if let (Some(bin_id), Some(spacing)) = (
                    self.predicted_tick_or_bin.or(self.confirmed_tick_or_bin),
                    self.spacing,
                ) {
                    let fee_rate = (self.fee_bps as u32) * 100_000; // bps to millionths
                    let dlmm = DlmmPoolState::new(spacing, bin_id, fee_rate);
                    return PoolSimulator::Dlmm(dlmm);
                }
                // Fallback to constant product if DLMM data incomplete
                tracing::warn!(
                    pool = %self.address,
                    "DLMM pool missing data, using constant product fallback"
                );
                PoolSimulator::ConstantProduct(self.predicted_virtual_state())
            }
            CurveType::ConstantProduct => {
                PoolSimulator::ConstantProduct(self.predicted_virtual_state())
            }
        }
    }

    /// Get a VirtualPoolState for simulation using CONFIRMED reserves
    pub fn confirmed_virtual_state(&self) -> VirtualPoolState {
        VirtualPoolState::new(
            self.confirmed_base_reserve,
            self.confirmed_quote_reserve,
            self.fee_bps,
        )
    }

    /// Get the spot price using PREDICTED state
    pub fn predicted_spot_price(&self) -> Option<f64> {
        if self.predicted_base_reserve == 0 {
            return None;
        }

        match self.curve {
            CurveType::ConcentratedLiquidity => {
                // Use predicted sqrt_price if available
                if let Some(sqrt_price) = self.predicted_sqrt_price {
                    let sqrt = (sqrt_price as f64) / ((1u128 << 64) as f64);
                    return Some(sqrt * sqrt);
                }
                // Fallback to predicted reserve ratio (less accurate for CLMM)
                None
            }
            CurveType::Dlmm => {
                // DLMM uses bin_id directly for price: price = 1.0001^bin_id
                if let Some(bin_id) = self.predicted_tick_or_bin {
                    let price = 1.0001_f64.powi(bin_id);
                    return Some(price);
                }
                None
            }
            CurveType::ConstantProduct => {
                Some(self.predicted_quote_reserve as f64 / self.predicted_base_reserve as f64)
            }
        }
    }

    /// How stale is the confirmed state?
    pub fn confirmed_state_age(&self) -> Duration {
        self.last_confirmed_at.elapsed()
    }

    /// Is the confirmed state fresh enough to trust?
    pub fn is_confirmed_state_fresh(&self, max_age: Duration) -> bool {
        self.confirmed_state_age() < max_age
    }
}

// =============================================================================
// PREDICTIVE STATE MANAGER
// =============================================================================

/// Central manager for all pool predictive states
///
/// This is the SINGLE SOURCE OF TRUTH for pool state in MEV evaluation.
/// Both ShredStream and Geyser feed into this manager.
pub struct PredictiveStateManager {
    /// Pool states indexed by address
    pools: RwLock<HashMap<Pubkey, PoolPredictiveState>>,
    /// Index: (mint_a, mint_b) → pool addresses
    pair_index: RwLock<HashMap<(Pubkey, Pubkey), Vec<Pubkey>>>,
    /// Index: vault address → (pool address, is_base_vault)
    vault_index: RwLock<HashMap<Pubkey, (Pubkey, bool)>>,
    /// Index: signature → pools affected by that pending swap
    /// This avoids re-parsing transactions on Geyser confirmation
    signature_to_pools: RwLock<HashMap<Signature, (Vec<Pubkey>, Instant)>>,
    /// Last cleanup time
    last_cleanup: RwLock<Instant>,
    /// Statistics
    pub stats: RwLock<PredictiveStateStats>,
    /// Tick array cache for accurate CLMM simulation
    /// Populated via Geyser subscriptions to tick array accounts
    tick_cache: Arc<TickArrayCache>,
    /// Bin array cache for accurate DLMM simulation
    /// Populated via Geyser subscriptions to bin array accounts
    bin_cache: Arc<BinArrayCache>,
}

#[derive(Debug, Default, Clone)]
pub struct PredictiveStateStats {
    pub pools_tracked: u64,
    pub pending_swaps_total: u64,
    pub pending_swaps_confirmed: u64,
    pub pending_swaps_expired: u64,
    pub pending_swaps_failed: u64,
    pub geyser_updates_received: u64,
    pub shredstream_swaps_received: u64,
    pub predictions_recalculated: u64,
    /// Signature index hits (avoided re-parsing)
    pub signature_index_hits: u64,
    /// Signature index misses (had to use fallback)
    pub signature_index_misses: u64,
    /// Vault account updates received from Geyser
    pub vault_updates_received: u64,
    /// Vault reserves successfully updated
    pub vault_reserves_updated: u64,
    /// CLMM pools that fell back to constant product (missing tick data)
    pub clmm_fallbacks: u64,
    /// DLMM pools that fell back to constant product (missing bin data)
    pub dlmm_fallbacks: u64,
}

impl PredictiveStateManager {
    pub fn new() -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            pair_index: RwLock::new(HashMap::new()),
            vault_index: RwLock::new(HashMap::new()),
            signature_to_pools: RwLock::new(HashMap::new()),
            last_cleanup: RwLock::new(Instant::now()),
            stats: RwLock::new(PredictiveStateStats::default()),
            tick_cache: Arc::new(TickArrayCache::new()),
            bin_cache: Arc::new(BinArrayCache::new()),
        }
    }

    /// Get the tick array cache for external access (Geyser subscription)
    pub fn tick_cache(&self) -> Arc<TickArrayCache> {
        Arc::clone(&self.tick_cache)
    }

    /// Get the bin array cache for external access (Geyser subscription)
    pub fn bin_cache(&self) -> Arc<BinArrayCache> {
        Arc::clone(&self.bin_cache)
    }

    // =========================================================================
    // POOL REGISTRATION
    // =========================================================================

    /// Register a new pool for tracking
    pub fn register_pool(&self, state: PoolPredictiveState) {
        let address = state.address;
        let base_mint = state.base_mint;
        let quote_mint = state.quote_mint;
        let vault_a = state.token_vault_a;
        let vault_b = state.token_vault_b;
        let dex = state.dex;
        let tick_arrays = state.tick_arrays.clone();
        let bin_arrays = state.bin_arrays.clone();
        let spacing = state.spacing.unwrap_or(64);

        // Check if this pair already has pools (for arb routing)
        let existing_pools_for_pair = {
            let index = self.pair_index.read().unwrap();
            index.get(&(base_mint, quote_mint)).map(|v| v.len()).unwrap_or(0)
        };

        // Add to pools map
        {
            let mut pools = self.pools.write().unwrap();
            pools.insert(address, state);
        }

        // Add to pair index (both directions)
        {
            let mut index = self.pair_index.write().unwrap();
            index
                .entry((base_mint, quote_mint))
                .or_default()
                .push(address);
            index
                .entry((quote_mint, base_mint))
                .or_default()
                .push(address);
        }

        // Add to vault index (if vaults are set)
        if vault_a != Pubkey::default() {
            let mut vault_idx = self.vault_index.write().unwrap();
            vault_idx.insert(vault_a, (address, true)); // is_base = true
            vault_idx.insert(vault_b, (address, false)); // is_base = false
        }

        // Register tick arrays for CLMM pools
        if !tick_arrays.is_empty() {
            let pool_type = match dex {
                DexType::OrcaWhirlpool => ClmmPoolType::OrcaWhirlpool,
                DexType::RaydiumClmm | DexType::PancakeSwapClmm => ClmmPoolType::RaydiumClmm,
                _ => ClmmPoolType::RaydiumClmm, // fallback
            };
            self.tick_cache
                .register_pool(address, &tick_arrays, spacing, pool_type);
        }

        // Register bin arrays for DLMM pools
        if !bin_arrays.is_empty() {
            self.bin_cache.register_pool(address, &bin_arrays, spacing);
        }

        // Update stats
        {
            let mut stats = self.stats.write().unwrap();
            stats.pools_tracked += 1;
        }

        // Log pool registration with pair info
        let new_count = existing_pools_for_pair + 1;
        if new_count > 1 {
            tracing::info!(
                "POOL REGISTERED: {} {:?} - pair {}/{} now has {} pools (ARB ROUTE AVAILABLE)",
                &address.to_string()[..8],
                dex,
                &base_mint.to_string()[..8],
                &quote_mint.to_string()[..8],
                new_count
            );
        } else {
            tracing::debug!(
                "POOL REGISTERED: {} {:?} base={} quote={}",
                &address.to_string()[..8],
                dex,
                &base_mint.to_string()[..8],
                &quote_mint.to_string()[..8]
            );
        }
    }

    /// Update pool metadata and re-index vaults
    pub fn update_pool_metadata(
        &self,
        pool: &Pubkey,
        token_vault_a: Pubkey,
        token_vault_b: Pubkey,
        amm_authority: Option<Pubkey>,
        amm_config: Option<Pubkey>,
        observation_state: Option<Pubkey>,
        tick_arrays: Vec<Pubkey>,
        bin_array_bitmap_extension: Option<Pubkey>,
        bin_arrays: Vec<Pubkey>,
        pool_authority: Option<Pubkey>,
    ) {
        // Get pool info for cache registration
        let (dex, spacing) = {
            let pools = self.pools.read().unwrap();
            pools.get(pool).map(|s| (s.dex, s.spacing.unwrap_or(64))).unwrap_or((DexType::RaydiumAmm, 64))
        };

        // Update pool state
        {
            let mut pools = self.pools.write().unwrap();
            if let Some(state) = pools.get_mut(pool) {
                state.set_metadata(
                    token_vault_a,
                    token_vault_b,
                    amm_authority,
                    amm_config,
                    observation_state,
                    tick_arrays.clone(),
                    bin_array_bitmap_extension,
                    bin_arrays.clone(),
                    pool_authority,
                );
            }
        }

        // Update vault index
        if token_vault_a != Pubkey::default() {
            let mut vault_idx = self.vault_index.write().unwrap();
            vault_idx.insert(token_vault_a, (*pool, true));
            vault_idx.insert(token_vault_b, (*pool, false));
        }

        // Register tick arrays for CLMM pools (re-register if arrays changed)
        if !tick_arrays.is_empty() {
            let pool_type = match dex {
                DexType::OrcaWhirlpool => ClmmPoolType::OrcaWhirlpool,
                DexType::RaydiumClmm | DexType::PancakeSwapClmm => ClmmPoolType::RaydiumClmm,
                _ => ClmmPoolType::RaydiumClmm,
            };
            self.tick_cache.register_pool(*pool, &tick_arrays, spacing, pool_type);
        }

        // Register bin arrays for DLMM pools (re-register if arrays changed)
        if !bin_arrays.is_empty() {
            self.bin_cache.register_pool(*pool, &bin_arrays, spacing);
        }
    }

    /// Get pool info for a vault address (for reserve updates from vault subscriptions)
    pub fn get_pool_for_vault(&self, vault: &Pubkey) -> Option<(Pubkey, bool)> {
        self.vault_index.read().unwrap().get(vault).cloned()
    }

    /// Get all tracked vault addresses (for Geyser subscription)
    pub fn all_vaults(&self) -> Vec<Pubkey> {
        self.vault_index.read().unwrap().keys().cloned().collect()
    }

    // =========================================================================
    // GEYSER INTEGRATION
    // =========================================================================

    /// Handle Geyser AccountUpdate for a vault address
    /// Looks up the pool from vault_index and updates the appropriate reserve side.
    /// Returns true if the vault was tracked and updated.
    pub fn on_geyser_vault_address_update(
        &self,
        vault_address: &Pubkey,
        balance: u64,
        slot: u64,
    ) -> bool {
        self.stats.write().unwrap().vault_updates_received += 1;

        // Look up which pool this vault belongs to
        let pool_info = self.vault_index.read().unwrap().get(vault_address).cloned();
        let Some((pool, is_base)) = pool_info else {
            return false;
        };

        // Get current reserves and update just one side
        let mut pools = self.pools.write().unwrap();
        let Some(state) = pools.get_mut(&pool) else {
            return false;
        };

        let (base_reserve, quote_reserve) = if is_base {
            (balance, state.confirmed_quote_reserve)
        } else {
            (state.confirmed_base_reserve, balance)
        };

        state.update_confirmed_reserves(base_reserve, quote_reserve, slot);
        self.stats.write().unwrap().vault_reserves_updated += 1;
        true
    }

    /// Handle Geyser AccountUpdate for a pool's vault (by pool address)
    pub fn on_geyser_vault_update(
        &self,
        pool: &Pubkey,
        is_base_vault: bool,
        balance: u64,
        slot: u64,
    ) {
        let mut pools = self.pools.write().unwrap();
        if let Some(state) = pools.get_mut(pool) {
            if is_base_vault {
                state.update_confirmed_reserves(balance, state.confirmed_quote_reserve, slot);
            } else {
                state.update_confirmed_reserves(state.confirmed_base_reserve, balance, slot);
            }

            self.stats.write().unwrap().geyser_updates_received += 1;
        }
    }

    /// Handle Geyser AccountUpdate for a pool's reserves (both at once)
    pub fn on_geyser_reserves_update(
        &self,
        pool: &Pubkey,
        base_reserve: u64,
        quote_reserve: u64,
        slot: u64,
    ) {
        let mut pools = self.pools.write().unwrap();
        if let Some(state) = pools.get_mut(pool) {
            state.update_confirmed_reserves(base_reserve, quote_reserve, slot);
            self.stats.write().unwrap().geyser_updates_received += 1;
        }
    }

    /// Handle Geyser TransactionUpdate (confirms pending swap)
    ///
    /// If `pool` is provided, confirms for that specific pool.
    /// If you have the signature but don't know the pool, use `confirm_by_signature` instead.
    pub fn on_geyser_transaction_confirmed(
        &self,
        signature: &Signature,
        pool: &Pubkey,
        success: bool,
    ) {
        let mut pools = self.pools.write().unwrap();
        if let Some(state) = pools.get_mut(pool) {
            if success {
                state.confirm_pending_swap(signature);
                self.stats.write().unwrap().pending_swaps_confirmed += 1;
            } else {
                state.fail_pending_swap(signature);
                self.stats.write().unwrap().pending_swaps_failed += 1;
            }
        }
        // Clean up signature index (drop locks first)
        drop(pools);
        self.remove_signature_from_index(signature);
    }

    /// Confirm a transaction by signature using the index (no re-parsing needed)
    /// Returns the pools that were confirmed, or None if signature wasn't tracked
    pub fn confirm_by_signature(
        &self,
        signature: &Signature,
        success: bool,
    ) -> Option<Vec<Pubkey>> {
        // Lookup pools from index
        let pools_for_sig = self.get_pools_for_signature(signature)?;

        self.stats.write().unwrap().signature_index_hits += 1;

        // Confirm/fail for each pool
        {
            let mut pools = self.pools.write().unwrap();
            for pool in &pools_for_sig {
                if let Some(state) = pools.get_mut(pool) {
                    if success {
                        state.confirm_pending_swap(signature);
                        self.stats.write().unwrap().pending_swaps_confirmed += 1;
                    } else {
                        state.fail_pending_swap(signature);
                        self.stats.write().unwrap().pending_swaps_failed += 1;
                    }
                }
            }
        }

        // Clean up signature index
        self.remove_signature_from_index(signature);

        Some(pools_for_sig)
    }

    /// Record a signature index miss (for stats tracking when fallback is used)
    pub fn record_signature_index_miss(&self) {
        self.stats.write().unwrap().signature_index_misses += 1;
    }

    // =========================================================================
    // SHREDSTREAM INTEGRATION
    // =========================================================================

    /// Handle ShredStream swap detection (early TX visibility)
    pub fn on_shredstream_swap_detected(&self, swap: &ParsedSwap, slot: u64, signature: Signature) {
        // Determine swap direction
        let base_to_quote = match swap.direction {
            Some(SwapDirection::AtoB) => true,
            Some(SwapDirection::BtoA) => false,
            None => return, // Can't predict without direction
        };

        let amount_in = match swap.amount_in {
            Some(amt) => amt,
            None => return, // Can't predict without amount
        };

        let pending = PendingSwap {
            signature,
            slot,
            pool: swap.pool,
            dex: swap.dex,
            amount_in,
            base_to_quote,
            detected_at: Instant::now(),
            simulated_output: None,
            status: PendingSwapStatus::Pending,
        };

        let mut pools = self.pools.write().unwrap();
        if let Some(state) = pools.get_mut(&swap.pool) {
            if state.add_pending_swap(pending) {
                // Add to signature → pools index for fast Geyser confirmation lookup
                {
                    let mut sig_index = self.signature_to_pools.write().unwrap();
                    sig_index
                        .entry(signature)
                        .or_insert_with(|| (Vec::new(), Instant::now()))
                        .0
                        .push(swap.pool);
                }
                self.stats.write().unwrap().shredstream_swaps_received += 1;
                self.stats.write().unwrap().pending_swaps_total += 1;
            }
        }
    }

    /// Get pools affected by a pending signature (fast lookup, no re-parsing)
    /// Returns None if the signature wasn't tracked (use fallback parsing)
    pub fn get_pools_for_signature(&self, signature: &Signature) -> Option<Vec<Pubkey>> {
        let sig_index = self.signature_to_pools.read().unwrap();
        sig_index.get(signature).map(|(pools, _)| pools.clone())
    }

    /// Remove a signature from the index (called after confirmation)
    fn remove_signature_from_index(&self, signature: &Signature) {
        let mut sig_index = self.signature_to_pools.write().unwrap();
        sig_index.remove(signature);
    }

    // =========================================================================
    // STATE ACCESS (for MEV Evaluation)
    // =========================================================================

    /// Get PREDICTED state for a pool (includes pending swaps)
    /// This is what MEV evaluation should use
    pub fn get_predicted_state(&self, pool: &Pubkey) -> Option<PoolPredictiveState> {
        self.maybe_run_cleanup();
        let pools = self.pools.read().unwrap();
        pools.get(pool).cloned()
    }

    /// Get PREDICTED virtual state for simulation
    pub fn get_predicted_virtual_state(&self, pool: &Pubkey) -> Option<VirtualPoolState> {
        self.maybe_run_cleanup();
        let pools = self.pools.read().unwrap();
        pools.get(pool).map(|s| s.predicted_virtual_state())
    }

    /// Get PREDICTED simulator with curve-specific math
    ///
    /// This returns a PoolSimulator that uses the correct math for the pool type:
    /// - CLMM pools use tick-based simulation (with cached tick data)
    /// - DLMM pools use bin-based simulation (with cached bin data)
    /// - AMM pools use constant product
    pub fn get_predicted_simulator(&self, pool: &Pubkey) -> Option<PoolSimulator> {
        self.maybe_run_cleanup();
        let pools = self.pools.read().unwrap();
        let state = pools.get(pool)?;

        // Build simulator with cached tick/bin data if available
        match state.curve {
            CurveType::ConcentratedLiquidity => {
                if let (Some(sqrt_price), Some(tick), Some(spacing), Some(liquidity)) = (
                    state.predicted_sqrt_price.or(state.confirmed_sqrt_price),
                    state.predicted_tick_or_bin.or(state.confirmed_tick_or_bin),
                    state.spacing,
                    state.active_liquidity,
                ) {
                    let fee_rate = (state.fee_bps as u32) * 100; // bps to millionths
                    let mut clmm =
                        ClmmPoolState::new(sqrt_price, tick, spacing, liquidity, fee_rate);

                    // Populate ticks from cache
                    let cached_ticks = self.tick_cache.get_pool_ticks(pool);
                    for tick_info in cached_ticks {
                        clmm.add_tick(tick_info.tick, tick_info.liquidity_net);
                    }

                    return Some(PoolSimulator::Clmm(clmm));
                }
                // Fallback to constant product if CLMM data incomplete
                self.stats.write().unwrap().clmm_fallbacks += 1;
                Some(PoolSimulator::ConstantProduct(state.predicted_virtual_state()))
            }
            CurveType::Dlmm => {
                if let (Some(bin_id), Some(spacing)) = (
                    state.predicted_tick_or_bin.or(state.confirmed_tick_or_bin),
                    state.spacing,
                ) {
                    let fee_rate = (state.fee_bps as u32) * 100_000; // bps to millionths
                    let mut dlmm = DlmmPoolState::new(spacing, bin_id, fee_rate);

                    // Populate bins from cache
                    let cached_bins = self.bin_cache.get_pool_bins_for_simulation(pool, spacing);
                    if cached_bins.is_empty() {
                        // Track DLMM fallback when no bin data available
                        self.stats.write().unwrap().dlmm_fallbacks += 1;
                    }
                    for bin in cached_bins {
                        dlmm.bins.insert(bin.id, bin);
                    }

                    return Some(PoolSimulator::Dlmm(dlmm));
                }
                // Fallback to constant product if DLMM data incomplete
                self.stats.write().unwrap().dlmm_fallbacks += 1;
                Some(PoolSimulator::ConstantProduct(state.predicted_virtual_state()))
            }
            CurveType::ConstantProduct => {
                Some(PoolSimulator::ConstantProduct(state.predicted_virtual_state()))
            }
        }
    }

    /// Get CONFIRMED virtual state (ground truth, no pending)
    pub fn get_confirmed_virtual_state(&self, pool: &Pubkey) -> Option<VirtualPoolState> {
        let pools = self.pools.read().unwrap();
        pools.get(pool).map(|s| s.confirmed_virtual_state())
    }

    /// Get pools for a token pair
    pub fn get_pools_for_pair(&self, mint_a: &Pubkey, mint_b: &Pubkey) -> Vec<Pubkey> {
        let index = self.pair_index.read().unwrap();
        index.get(&(*mint_a, *mint_b)).cloned().unwrap_or_default()
    }

    /// Find an alternate pool for a token pair (excluding the victim pool)
    /// Returns the pool with valid metadata if available
    pub fn find_alternate_pool(
        &self,
        victim_pool: &Pubkey,
        mint_a: &Pubkey,
        mint_b: &Pubkey,
    ) -> Option<PoolPredictiveState> {
        let pool_addresses = self.get_pools_for_pair(mint_a, mint_b);
        let pools = self.pools.read().unwrap();

        for addr in pool_addresses {
            if addr != *victim_pool {
                if let Some(state) = pools.get(&addr) {
                    // Prefer pools with valid metadata
                    if state.has_metadata() {
                        return Some(state.clone());
                    }
                }
            }
        }

        // If no pool with metadata, return any alternate
        for addr in self.get_pools_for_pair(mint_a, mint_b) {
            if addr != *victim_pool {
                if let Some(state) = pools.get(&addr) {
                    return Some(state.clone());
                }
            }
        }

        None
    }

    /// Get predicted spot price for a pool
    pub fn get_predicted_spot_price(&self, pool: &Pubkey) -> Option<f64> {
        self.maybe_run_cleanup();
        let pools = self.pools.read().unwrap();
        pools.get(pool).and_then(|s| s.predicted_spot_price())
    }

    /// Get pending swap count for a pool
    pub fn pending_swap_count(&self, pool: &Pubkey) -> usize {
        let pools = self.pools.read().unwrap();
        pools.get(pool).map(|s| s.pending_swap_count).unwrap_or(0)
    }

    /// Check if a pool is registered
    pub fn has_pool(&self, pool: &Pubkey) -> bool {
        let pools = self.pools.read().unwrap();
        pools.contains_key(pool)
    }

    /// Get pool mints (base_mint, quote_mint)
    pub fn get_pool_mints(&self, pool: &Pubkey) -> Option<(Pubkey, Pubkey)> {
        let pools = self.pools.read().unwrap();
        pools.get(pool).map(|s| (s.base_mint, s.quote_mint))
    }

    /// Get pool reserves (predicted_base, predicted_quote)
    pub fn get_pool_reserves(&self, pool: &Pubkey) -> Option<(u64, u64)> {
        self.maybe_run_cleanup();
        let pools = self.pools.read().unwrap();
        pools
            .get(pool)
            .map(|s| (s.predicted_base_reserve, s.predicted_quote_reserve))
    }

    /// Get pool fee in basis points
    pub fn get_pool_fee_bps(&self, pool: &Pubkey) -> Option<u16> {
        let pools = self.pools.read().unwrap();
        pools.get(pool).map(|s| s.fee_bps)
    }

    /// Get pool DEX type
    pub fn get_pool_dex(&self, pool: &Pubkey) -> Option<DexType> {
        let pools = self.pools.read().unwrap();
        pools.get(pool).map(|s| s.dex)
    }

    /// Get number of pools tracked
    pub fn pool_count(&self) -> usize {
        let pools = self.pools.read().unwrap();
        pools.len()
    }

    /// Update CLMM state for a pool (sqrt_price, tick, liquidity)
    pub fn update_clmm_state(
        &self,
        pool: &Pubkey,
        sqrt_price: Option<u128>,
        tick_or_bin: i32,
        spacing: u16,
        liquidity: Option<u128>,
        slot: u64,
    ) {
        let mut pools = self.pools.write().unwrap();
        if let Some(state) = pools.get_mut(pool) {
            state.update_confirmed_clmm(sqrt_price, tick_or_bin, spacing, liquidity, slot);
        }
    }

    // =========================================================================
    // MAINTENANCE
    // =========================================================================

    /// Run cleanup if enough time has passed
    fn maybe_run_cleanup(&self) {
        let should_run = {
            let last = self.last_cleanup.read().unwrap();
            last.elapsed() > CLEANUP_INTERVAL
        };

        if should_run {
            self.run_cleanup();
        }
    }

    /// Expire old pending swaps and clean up
    fn run_cleanup(&self) {
        let mut total_expired = 0;
        {
            let mut pools = self.pools.write().unwrap();
            for state in pools.values_mut() {
                total_expired += state.expire_old_swaps();
            }
        }

        // Also clean up stale signature index entries
        let mut sig_expired = 0;
        {
            let mut sig_index = self.signature_to_pools.write().unwrap();
            let now = Instant::now();
            sig_index.retain(|_, (_, created_at)| {
                let keep = now.duration_since(*created_at) < PENDING_SWAP_TTL;
                if !keep {
                    sig_expired += 1;
                }
                keep
            });
        }

        if total_expired > 0 || sig_expired > 0 {
            self.stats.write().unwrap().pending_swaps_expired += total_expired as u64;
            tracing::debug!(
                "Expired {} stale pending swaps, {} signature index entries",
                total_expired,
                sig_expired
            );
        }

        *self.last_cleanup.write().unwrap() = Instant::now();
    }

    /// Get statistics
    pub fn stats(&self) -> PredictiveStateStats {
        self.stats.read().unwrap().clone()
    }

    /// Get statistics as a formatted summary string
    pub fn stats_summary(&self) -> String {
        let s = self.stats.read().unwrap();
        format!(
            "pools={} pending={} confirmed={} expired={} failed={} geyser_updates={} shred_swaps={} recalcs={} sig_hits={} sig_misses={}",
            s.pools_tracked,
            s.pending_swaps_total,
            s.pending_swaps_confirmed,
            s.pending_swaps_expired,
            s.pending_swaps_failed,
            s.geyser_updates_received,
            s.shredstream_swaps_received,
            s.predictions_recalculated,
            s.signature_index_hits,
            s.signature_index_misses
        )
    }

    /// Get pair index statistics - critical for cross-pool arb routing
    /// Shows how many token pairs have multiple pools available
    pub fn pair_index_stats(&self) -> String {
        let index = self.pair_index.read().unwrap();
        let total_pairs = index.len();
        let multi_pool_pairs = index.values().filter(|pools| pools.len() > 1).count();
        let max_pools_for_pair = index.values().map(|p| p.len()).max().unwrap_or(0);
        
        // Count pairs by pool count
        let pairs_with_2 = index.values().filter(|p| p.len() == 2).count();
        let pairs_with_3_plus = index.values().filter(|p| p.len() >= 3).count();
        
        format!(
            "total_pairs={} multi_pool={} max_pools={} pairs_2pools={} pairs_3plus={}",
            total_pairs / 2, // Divide by 2 because we index both directions
            multi_pool_pairs / 2,
            max_pools_for_pair,
            pairs_with_2 / 2,
            pairs_with_3_plus / 2
        )
    }

    /// Get the current size of the signature index
    pub fn signature_index_size(&self) -> usize {
        self.signature_to_pools.read().unwrap().len()
    }

    /// Get total pending swaps across all pools
    pub fn total_pending_swaps(&self) -> usize {
        let pools = self.pools.read().unwrap();
        pools.values().map(|s| s.pending_swap_count).sum()
    }
}

impl Default for PredictiveStateManager {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// HELPER: SIMULATE ARB WITH PREDICTED STATE
// =============================================================================

/// Simulate a cross-pool backrun using PREDICTED state
///
/// This is the key function that uses state prediction for accurate profit calculation.
///
/// # Flow
/// 1. Get predicted state for both pools (includes pending swaps)
/// 2. Simulate victim's swap on pool X (from predicted state)
/// 3. Simulate our arb legs on predicted states
/// 4. Calculate profit accounting for the victim + pending swaps
pub fn simulate_backrun_with_prediction(
    manager: &PredictiveStateManager,
    victim_pool: &Pubkey,
    alternate_pool: &Pubkey,
    victim_amount: u64,
    victim_base_to_quote: bool,
    borrow_amount: u64,
    borrow_is_base: bool,
    flash_fee_bps: u16,
) -> Option<BackrunSimulationResult> {
    // Get PREDICTED states (includes all pending swaps)
    let pool_x = manager.get_predicted_virtual_state(victim_pool)?;
    let pool_y = manager.get_predicted_virtual_state(alternate_pool)?;

    // Now simulate: victim on pool_x, then our arb
    // 1. Victim's swap on pool X (predicted state)
    let (_, pool_x_after_victim) = pool_x.simulate_swap(victim_amount, victim_base_to_quote);

    // 2. Our leg 1: Swap on Pool Y (borrow_token → intermediate)
    let leg1_base_to_quote = borrow_is_base;
    let (leg1_output, _) = pool_y.simulate_swap(borrow_amount, leg1_base_to_quote);
    if leg1_output == 0 {
        return None;
    }

    // 3. Our leg 2: Swap on Pool X post-victim (intermediate → borrow)
    let leg2_base_to_quote = !borrow_is_base;
    let (leg2_output, _) = pool_x_after_victim.simulate_swap(leg1_output, leg2_base_to_quote);
    if leg2_output == 0 {
        return None;
    }

    // 4. Calculate profit
    let flash_fee = (borrow_amount as u128 * flash_fee_bps as u128 / 10_000) as u64;
    let repay_amount = borrow_amount.saturating_add(flash_fee);
    let profit = leg2_output.saturating_sub(repay_amount);

    Some(BackrunSimulationResult {
        profit,
        borrow_amount,
        leg1_output,
        leg2_output,
        flash_loan_fee: flash_fee,
        pending_swaps_in_victim_pool: manager.pending_swap_count(victim_pool),
        pending_swaps_in_alternate_pool: manager.pending_swap_count(alternate_pool),
    })
}

/// Result of simulating a backrun with state prediction
#[derive(Clone, Debug)]
pub struct BackrunSimulationResult {
    pub profit: u64,
    pub borrow_amount: u64,
    pub leg1_output: u64,
    pub leg2_output: u64,
    pub flash_loan_fee: u64,
    /// How many pending swaps were factored into victim pool prediction
    pub pending_swaps_in_victim_pool: usize,
    /// How many pending swaps were factored into alternate pool prediction
    pub pending_swaps_in_alternate_pool: usize,
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_pool(address: Pubkey, base: u64, quote: u64) -> PoolPredictiveState {
        PoolPredictiveState::new(
            address,
            DexType::RaydiumAmm,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            base,
            quote,
            9,
            6,
            30, // 0.3% fee
            CurveType::ConstantProduct,
            100,
        )
    }

    #[test]
    fn test_pending_swap_affects_prediction() {
        let pool_addr = Pubkey::new_unique();
        let mut pool = make_test_pool(pool_addr, 1_000_000_000, 1_000_000);

        // Initial state: confirmed = predicted
        assert_eq!(pool.confirmed_base_reserve, pool.predicted_base_reserve);

        // Add a pending swap
        let pending = PendingSwap {
            signature: Signature::new_unique(),
            slot: 101,
            pool: pool_addr,
            dex: DexType::RaydiumAmm,
            amount_in: 100_000_000, // 0.1 SOL
            base_to_quote: true,
            detected_at: Instant::now(),
            simulated_output: None,
            status: PendingSwapStatus::Pending,
        };

        pool.add_pending_swap(pending);

        // Now predicted should differ from confirmed
        assert!(pool.predicted_base_reserve > pool.confirmed_base_reserve);
        assert!(pool.predicted_quote_reserve < pool.confirmed_quote_reserve);
        assert_eq!(pool.pending_swap_count, 1);
    }

    #[test]
    fn test_geyser_confirmation_removes_pending() {
        let pool_addr = Pubkey::new_unique();
        let mut pool = make_test_pool(pool_addr, 1_000_000_000, 1_000_000);

        let sig = Signature::new_unique();
        let pending = PendingSwap {
            signature: sig,
            slot: 101,
            pool: pool_addr,
            dex: DexType::RaydiumAmm,
            amount_in: 100_000_000,
            base_to_quote: true,
            detected_at: Instant::now(),
            simulated_output: None,
            status: PendingSwapStatus::Pending,
        };

        pool.add_pending_swap(pending);
        assert_eq!(pool.pending_swap_count, 1);

        // Simulate Geyser confirmation
        pool.confirm_pending_swap(&sig);
        assert_eq!(pool.pending_swap_count, 0);

        // Update confirmed reserves (as Geyser would)
        pool.update_confirmed_reserves(1_100_000_000, 900_000, 102);

        // Now confirmed = predicted again (no pending)
        assert_eq!(pool.confirmed_base_reserve, pool.predicted_base_reserve);
    }

    #[test]
    fn test_manager_integration() {
        let manager = PredictiveStateManager::new();

        let pool_addr = Pubkey::new_unique();
        let pool = make_test_pool(pool_addr, 1_000_000_000, 1_000_000);
        manager.register_pool(pool);

        // Get predicted state
        let state = manager.get_predicted_state(&pool_addr).unwrap();
        assert_eq!(state.predicted_base_reserve, 1_000_000_000);

        // Simulate ShredStream swap
        let swap = ParsedSwap {
            dex: DexType::RaydiumAmm,
            pool: pool_addr,
            input_mint: None,
            output_mint: None,
            amount_in: Some(100_000_000),
            min_amount_out: None,
            direction: Some(SwapDirection::AtoB),
            user: Pubkey::new_unique(),
            instruction_index: 0,
            amount_specified_is_input: Some(true),
            sqrt_price_limit: None,
        };

        manager.on_shredstream_swap_detected(&swap, 101, Signature::new_unique());

        // Predicted state should now differ
        let state = manager.get_predicted_state(&pool_addr).unwrap();
        assert!(state.predicted_base_reserve > state.confirmed_base_reserve);
    }
}
