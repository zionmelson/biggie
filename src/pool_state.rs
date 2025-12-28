//! Pool State Manager
//!
//! Manages pool state from two sources with different latency/reliability tradeoffs:
//! - **Turbine/ShredStream**: Fast (~0.5-2ms) but derived from TX parsing
//! - **Geyser**: Authoritative (~5-15ms) direct vault balance updates
//!
//! # IMPORTANT: Shreds Are Post-Execution
//!
//! Transactions seen in shreds have ALREADY BEEN EXECUTED. We are not "predicting"
//! future state - we are updating our view of current state as fast as possible.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                      PoolStateManager                           │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                                                                 │
//! │  FAST PATH (Turbine ~0.5-2ms):                                  │
//! │  • Parse swap TXs from shreds                                   │
//! │  • Simulate reserve changes                                     │
//! │  • Update pool state immediately                                │
//! │  • TRIGGER: evaluate arb for affected pair                      │
//! │                                                                 │
//! │  RELIABLE PATH (Geyser ~5-15ms):                                │
//! │  • Receive actual vault balances                                │
//! │  • Update pool state authoritatively                            │
//! │  • TRIGGER: evaluate arb for affected pair                      │
//! │                                                                 │
//! │  PAIR INDEX:                                                    │
//! │  • (base_mint, quote_mint) → [pool1, pool2, ...]                │
//! │  • Enables cross-pool arb evaluation                            │
//! │                                                                 │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Timing Reality
//!
//! ```text
//! Block N produced by leader:
//! t=0ms      Swap executed in block (we have no visibility)
//! t=0.5-2ms  Turbine: Shreds arrive → parse → update state
//! t=1-3ms    Arb evaluation triggered → opportunity found
//! t=2-5ms    Arb TX submitted to Jito for Block N+1
//! t=5-15ms   Geyser: Vault update confirms state
//! t=400ms    Block N+1: Our arb TX included (if we won)
//! ```

use crate::amm_math::{CurveType, PoolSimulator, VirtualPoolState};
use crate::bin_array_cache::BinArrayCache;
use crate::clmm_math::ClmmPoolState;
use crate::constants::{TOKEN_PROGRAM, TOKEN_2022_PROGRAM};
use crate::dlmm_math::DlmmPoolState;
use crate::shredstream::programs::DexType;
use crate::tick_array_cache::{ClmmPoolType, TickArrayCache};
use parking_lot::RwLock;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Route information for 3-hop arbitrage through an anchor mint
#[derive(Clone, Debug)]
pub struct ThreeHopRoute {
    /// Pool A: routes anchor → token_a
    pub pool_a: PoolState,
    /// Pool B: routes token_b → anchor
    pub pool_b: PoolState,
    /// The anchor mint used for flash loan (SOL, USDC, etc.)
    pub anchor_mint: Pubkey,
    /// Swap direction for leg 1 (anchor → token_a)
    pub leg1_base_to_quote: bool,
    /// Swap direction for leg 3 (token_b → anchor)
    pub leg3_base_to_quote: bool,
}

// =============================================================================
// POOL STATE
// =============================================================================

/// Complete state for a single pool with prediction capability
#[derive(Clone, Debug)]
pub struct PoolState {
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
    
    // === TOKEN PROGRAM TRACKING (for Token-2022 support) ===
    /// Token program for token A (SPL Token or Token-2022)
    pub token_program_a: Pubkey,
    /// Token program for token B (SPL Token or Token-2022)
    pub token_program_b: Pubkey,
    /// Transfer fee in bps for token A (Token-2022 only, 0 for regular SPL)
    pub transfer_fee_bps_a: u16,
    /// Transfer fee in bps for token B (Token-2022 only, 0 for regular SPL)  
    pub transfer_fee_bps_b: u16,
}

impl PoolState {
    /// Create a new pool state
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
            // Default to SPL Token program - will be updated via set_token_programs()
            token_program_a: TOKEN_PROGRAM,
            token_program_b: TOKEN_PROGRAM,
            transfer_fee_bps_a: 0,
            transfer_fee_bps_b: 0,
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

    /// Set token program info for Token-2022 support
    pub fn set_token_programs(
        &mut self,
        token_program_a: Pubkey,
        token_program_b: Pubkey,
        transfer_fee_bps_a: u16,
        transfer_fee_bps_b: u16,
    ) {
        self.token_program_a = token_program_a;
        self.token_program_b = token_program_b;
        self.transfer_fee_bps_a = transfer_fee_bps_a;
        self.transfer_fee_bps_b = transfer_fee_bps_b;
    }

    /// Check if either token uses Token-2022
    pub fn uses_token_2022(&self) -> bool {
        self.token_program_a == TOKEN_2022_PROGRAM || self.token_program_b == TOKEN_2022_PROGRAM
    }

    /// Get total transfer fees for a round-trip (buy + sell) in bps
    pub fn total_transfer_fee_bps(&self) -> u16 {
        // For arb: we transfer in token A, swap, transfer in token B, swap back
        // Each token may charge transfer fee on each transfer
        self.transfer_fee_bps_a.saturating_add(self.transfer_fee_bps_b)
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
    }

    /// Get a VirtualPoolState for simulation using confirmed reserves.
    /// 
    /// NOTE: For CLMM/DLMM pools, use PoolStateManager::get_simulator() instead
    /// which populates tick/bin data from the cache.
    pub fn virtual_state(&self) -> VirtualPoolState {
        VirtualPoolState::new(
            self.confirmed_base_reserve,
            self.confirmed_quote_reserve,
            self.fee_bps,
        )
    }

    /// Get the spot price using confirmed state
    pub fn spot_price(&self) -> Option<f64> {
        if self.confirmed_base_reserve == 0 {
            return None;
        }

        match self.curve {
            CurveType::ConcentratedLiquidity => {
                // Use confirmed sqrt_price
                if let Some(sqrt_price) = self.confirmed_sqrt_price {
                    let sqrt = (sqrt_price as f64) / ((1u128 << 64) as f64);
                    return Some(sqrt * sqrt);
                }
                None
            }
            CurveType::Dlmm => {
                // DLMM uses bin_id directly for price: price = 1.0001^bin_id
                if let Some(bin_id) = self.confirmed_tick_or_bin {
                    let price = 1.0001_f64.powi(bin_id);
                    return Some(price);
                }
                None
            }
            CurveType::ConstantProduct => {
                Some(self.confirmed_quote_reserve as f64 / self.confirmed_base_reserve as f64)
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
// POOL STATE MANAGER
// =============================================================================

/// Central manager for all pool states
///
/// This is the SINGLE SOURCE OF TRUTH for pool state in arbitrage evaluation.
/// Both Turbine and Geyser feed into this manager.
pub struct PoolStateManager {
    /// Pool states indexed by address
    pools: RwLock<HashMap<Pubkey, PoolState>>,
    /// Index: (mint_a, mint_b) → pool addresses
    pair_index: RwLock<HashMap<(Pubkey, Pubkey), Vec<Pubkey>>>,
    /// Index: vault address → (pool address, is_base_vault)
    vault_index: RwLock<HashMap<Pubkey, (Pubkey, bool)>>,
    /// Statistics
    pub stats: RwLock<PoolStateStats>,
    /// Tick array cache for accurate CLMM simulation
    tick_cache: Arc<TickArrayCache>,
    /// Bin array cache for accurate DLMM simulation
    bin_cache: Arc<BinArrayCache>,
}

#[derive(Debug, Default, Clone)]
pub struct PoolStateStats {
    pub pools_tracked: u64,
    pub geyser_updates_received: u64,
    /// Vault account updates received from Geyser
    pub vault_updates_received: u64,
    /// Vault reserves successfully updated
    pub vault_reserves_updated: u64,
}

impl PoolStateManager {
    pub fn new() -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            pair_index: RwLock::new(HashMap::new()),
            vault_index: RwLock::new(HashMap::new()),
            stats: RwLock::new(PoolStateStats::default()),
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
    pub fn register_pool(&self, state: PoolState) {
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
            let index = self.pair_index.read();
            index.get(&(base_mint, quote_mint)).map(|v| v.len()).unwrap_or(0)
        };

        // Add to pools map
        {
            let mut pools = self.pools.write();
            pools.insert(address, state);
        }

        // Add to pair index (both directions)
        {
            let mut index = self.pair_index.write();
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
            let mut vault_idx = self.vault_index.write();
            vault_idx.insert(vault_a, (address, true)); // is_base = true
            vault_idx.insert(vault_b, (address, false)); // is_base = false
            
            // Log vault_index size periodically
            let idx_size = vault_idx.len();
            if idx_size % 100 == 0 {
                tracing::debug!("vault_index now has {} entries", idx_size);
            }
        } else {
            tracing::warn!(
                "Pool {} has default vault addresses - won't receive Geyser updates",
                &address.to_string()[..8]
            );
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
            let mut stats = self.stats.write();
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
            let pools = self.pools.read();
            pools.get(pool).map(|s| (s.dex, s.spacing.unwrap_or(64))).unwrap_or((DexType::RaydiumAmm, 64))
        };

        // Update pool state
        {
            let mut pools = self.pools.write();
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
            let mut vault_idx = self.vault_index.write();
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
        self.vault_index.read().get(vault).cloned()
    }

    /// Get all tracked vault addresses (for Geyser subscription)
    pub fn all_vaults(&self) -> Vec<Pubkey> {
        self.vault_index.read().keys().cloned().collect()
    }

    /// Get a snapshot of the vault_index for transaction-based balance extraction
    pub fn vault_index_snapshot(&self) -> HashMap<Pubkey, (Pubkey, bool)> {
        self.vault_index.read().clone()
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
        let updates_rx = {
            let mut stats = self.stats.write();
            stats.vault_updates_received += 1;
            stats.vault_updates_received
        };

        // Look up which pool this vault belongs to
        let vault_idx = self.vault_index.read();
        let pool_info = vault_idx.get(vault_address).cloned();
        let vault_idx_size = vault_idx.len();
        drop(vault_idx);
        
        let Some((pool, is_base)) = pool_info else {
            // Log first few misses to diagnose
            if updates_rx <= 5 {
                tracing::warn!(
                    "Vault {} not in vault_index (size={}). First {} updates all missed!",
                    vault_address,
                    vault_idx_size,
                    updates_rx
                );
            }
            return false;
        };

        // Get current reserves and update just one side
        let mut pools = self.pools.write();
        let Some(state) = pools.get_mut(&pool) else {
            return false;
        };

        let (base_reserve, quote_reserve) = if is_base {
            (balance, state.confirmed_quote_reserve)
        } else {
            (state.confirmed_base_reserve, balance)
        };

        state.update_confirmed_reserves(base_reserve, quote_reserve, slot);
        self.stats.write().vault_reserves_updated += 1;
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
        let mut pools = self.pools.write();
        if let Some(state) = pools.get_mut(pool) {
            if is_base_vault {
                state.update_confirmed_reserves(balance, state.confirmed_quote_reserve, slot);
            } else {
                state.update_confirmed_reserves(state.confirmed_base_reserve, balance, slot);
            }

            self.stats.write().geyser_updates_received += 1;
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
        let mut pools = self.pools.write();
        if let Some(state) = pools.get_mut(pool) {
            state.update_confirmed_reserves(base_reserve, quote_reserve, slot);
            self.stats.write().geyser_updates_received += 1;
        }
    }

    // =========================================================================
    // STATE ACCESS (for Arb Evaluation)
    // =========================================================================

    /// Get pool state
    pub fn get_pool_state(&self, pool: &Pubkey) -> Option<PoolState> {
        let pools = self.pools.read();
        pools.get(pool).cloned()
    }

    /// Get virtual state for simulation
    pub fn get_virtual_state(&self, pool: &Pubkey) -> Option<VirtualPoolState> {
        let pools = self.pools.read();
        pools.get(pool).map(|s| s.virtual_state())
    }

    /// Get simulator with curve-specific math
    ///
    /// This returns a PoolSimulator that uses the correct math for the pool type:
    /// - CLMM pools use tick-based simulation (with cached tick data)
    /// - DLMM pools use bin-based simulation (with cached bin data)
    /// - AMM pools use constant product
    /// - DAMM v2 uses CLMM math but with single continuous liquidity range (no tick arrays)
    pub fn get_simulator(&self, pool: &Pubkey) -> Option<PoolSimulator> {
        let pools = self.pools.read();
        let state = pools.get(pool)?;

        // Build simulator with cached tick/bin data if available
        match state.curve {
            CurveType::ConcentratedLiquidity => {
                // For DAMM v2, we need sqrt_price and liquidity but NOT tick arrays
                // (DAMM uses single continuous range, unlike Whirlpool/Raydium CLMM)
                let is_damm = state.dex == DexType::MeteoraDamm;
                
                if let (Some(sqrt_price), Some(liquidity)) = (
                    state.confirmed_sqrt_price,
                    state.active_liquidity,
                ) {
                    // For DAMM, use defaults if tick/spacing not set (single continuous range)
                    let tick = state.confirmed_tick_or_bin.unwrap_or(0);
                    let spacing = state.spacing.unwrap_or(1);
                    let fee_rate = (state.fee_bps as u32) * 100;
                    let mut clmm =
                        ClmmPoolState::new(sqrt_price, tick, spacing, liquidity, fee_rate);

                    // Populate ticks from cache (only relevant for non-DAMM CLMMs)
                    // DAMM v2 has all liquidity in one range, so tick cache will be empty
                    if !is_damm {
                        let cached_ticks = self.tick_cache.get_pool_ticks(pool);
                        for tick_info in cached_ticks {
                            clmm.add_tick(tick_info.tick, tick_info.liquidity_net);
                        }
                    }

                    return Some(PoolSimulator::Clmm(clmm));
                }
                
                // CRITICAL: For CLMM pools, falling back to constant product gives WRONG results!
                // Log a warning and return None instead of silently using wrong math.
                tracing::warn!(
                    pool = %pool,
                    dex = ?state.dex,
                    has_sqrt_price = state.confirmed_sqrt_price.is_some(),
                    has_liquidity = state.active_liquidity.is_some(),
                    "CLMM pool missing sqrt_price or liquidity - cannot simulate accurately"
                );
                None
            }
            CurveType::Dlmm => {
                if let (Some(bin_id), Some(spacing)) = (
                    state.confirmed_tick_or_bin,
                    state.spacing,
                ) {
                    let fee_rate = (state.fee_bps as u32) * 100_000;
                    let mut dlmm = DlmmPoolState::new(spacing, bin_id, fee_rate);

                    // Populate bins from cache
                    let cached_bins = self.bin_cache.get_pool_bins_for_simulation(pool, spacing);
                    for bin in cached_bins {
                        dlmm.bins.insert(bin.id, bin);
                    }

                    return Some(PoolSimulator::Dlmm(dlmm));
                }
                // CRITICAL: For DLMM pools, falling back to constant product gives WRONG results!
                tracing::warn!(
                    pool = %pool,
                    has_bin_id = state.confirmed_tick_or_bin.is_some(),
                    has_spacing = state.spacing.is_some(),
                    "DLMM pool missing bin_id or spacing - cannot simulate accurately"
                );
                None
            }
            CurveType::ConstantProduct => {
                Some(PoolSimulator::ConstantProduct(state.virtual_state()))
            }
        }
    }

    /// Get pools for a token pair
    pub fn get_pools_for_pair(&self, mint_a: &Pubkey, mint_b: &Pubkey) -> Vec<Pubkey> {
        let index = self.pair_index.read();
        index.get(&(*mint_a, *mint_b)).cloned().unwrap_or_default()
    }

    /// Find an alternate pool for a token pair (excluding the source pool)
    /// Returns the pool with valid metadata if available
    pub fn find_alternate_pool(
        &self,
        source_pool: &Pubkey,
        mint_a: &Pubkey,
        mint_b: &Pubkey,
    ) -> Option<PoolState> {
        let pool_addresses = self.get_pools_for_pair(mint_a, mint_b);
        let pools = self.pools.read();

        for addr in pool_addresses {
            if addr != *source_pool {
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
            if addr != *source_pool {
                if let Some(state) = pools.get(&addr) {
                    return Some(state.clone());
                }
            }
        }

        None
    }

    /// Find ALL alternate pools for a token pair (excluding the source pool)
    /// Returns all pools with valid metadata for multi-pool route evaluation
    pub fn find_all_alternate_pools(
        &self,
        source_pool: &Pubkey,
        mint_a: &Pubkey,
        mint_b: &Pubkey,
    ) -> Vec<PoolState> {
        let pool_addresses = self.get_pools_for_pair(mint_a, mint_b);
        let pools = self.pools.read();
        let mut result = Vec::new();

        for addr in pool_addresses {
            if addr != *source_pool {
                if let Some(state) = pools.get(&addr) {
                    // Only include pools with valid metadata
                    if state.has_metadata() {
                        result.push(state.clone());
                    }
                }
            }
        }

        result
    }

    /// Find routing through an anchor mint for 3-hop arbitrage.
    ///
    /// When a swap occurs token_a → token_b and neither is an anchor:
    /// 1. Find a pool that routes anchor → token_a (Pool A)
    /// 2. Use source pool for token_a → token_b (Pool X)
    /// 3. Find a pool that routes token_b → anchor (Pool B)
    ///
    /// Returns: (pool_a, pool_b, anchor_mint, leg1_base_to_quote, leg3_base_to_quote)
    pub fn find_anchor_route(
        &self,
        source_pool: &Pubkey,
        token_a: &Pubkey,
        token_b: &Pubkey,
        anchor_mints: &[Pubkey],
    ) -> Option<ThreeHopRoute> {
        let pools = self.pools.read();
        let index = self.pair_index.read();

        for anchor in anchor_mints {
            // Skip if either token IS the anchor (should use 2-hop instead)
            if token_a == anchor || token_b == anchor {
                continue;
            }

            // Find Pool A: anchor/token_a pair
            let pool_a_candidates = index.get(&(*anchor, *token_a))
                .or_else(|| index.get(&(*token_a, *anchor)));
            
            let pool_a = pool_a_candidates.and_then(|addrs| {
                addrs.iter()
                    .filter(|addr| *addr != source_pool)
                    .find_map(|addr| {
                        pools.get(addr).filter(|s| s.has_metadata()).cloned()
                    })
            });

            // Find Pool B: token_b/anchor pair
            let pool_b_candidates = index.get(&(*token_b, *anchor))
                .or_else(|| index.get(&(*anchor, *token_b)));
            
            let pool_b = pool_b_candidates.and_then(|addrs| {
                addrs.iter()
                    .filter(|addr| *addr != source_pool)
                    .find_map(|addr| {
                        pools.get(addr).filter(|s| s.has_metadata()).cloned()
                    })
            });

            if let (Some(pool_a_state), Some(pool_b_state)) = (pool_a, pool_b) {
                // Determine swap directions for each leg
                // Leg 1: ANCHOR → token_a on pool_a
                let leg1_base_to_quote = pool_a_state.base_mint == *anchor;
                
                // Leg 3: token_b → ANCHOR on pool_b
                let leg3_base_to_quote = pool_b_state.base_mint == *token_b;

                return Some(ThreeHopRoute {
                    pool_a: pool_a_state,
                    pool_b: pool_b_state,
                    anchor_mint: *anchor,
                    leg1_base_to_quote,
                    leg3_base_to_quote,
                });
            }
        }

        None
    }

    /// Get spot price for a pool
    pub fn get_spot_price(&self, pool: &Pubkey) -> Option<f64> {
        let pools = self.pools.read();
        pools.get(pool).and_then(|s| s.spot_price())
    }

    /// Check if a pool is registered
    pub fn has_pool(&self, pool: &Pubkey) -> bool {
        let pools = self.pools.read();
        pools.contains_key(pool)
    }

    /// Get pool mints (base_mint, quote_mint)
    pub fn get_pool_mints(&self, pool: &Pubkey) -> Option<(Pubkey, Pubkey)> {
        let pools = self.pools.read();
        pools.get(pool).map(|s| (s.base_mint, s.quote_mint))
    }

    /// Get pool reserves (confirmed_base, confirmed_quote)
    pub fn get_pool_reserves(&self, pool: &Pubkey) -> Option<(u64, u64)> {
        let pools = self.pools.read();
        pools
            .get(pool)
            .map(|s| (s.confirmed_base_reserve, s.confirmed_quote_reserve))
    }

    /// Get pool fee in basis points
    pub fn get_pool_fee_bps(&self, pool: &Pubkey) -> Option<u16> {
        let pools = self.pools.read();
        pools.get(pool).map(|s| s.fee_bps)
    }

    /// Get pool DEX type
    pub fn get_pool_dex(&self, pool: &Pubkey) -> Option<DexType> {
        let pools = self.pools.read();
        pools.get(pool).map(|s| s.dex)
    }

    /// Get number of pools tracked
    pub fn pool_count(&self) -> usize {
        let pools = self.pools.read();
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
        let mut pools = self.pools.write();
        if let Some(state) = pools.get_mut(pool) {
            state.update_confirmed_clmm(sqrt_price, tick_or_bin, spacing, liquidity, slot);
        }
    }

    /// Get statistics
    pub fn stats(&self) -> PoolStateStats {
        self.stats.read().clone()
    }

    /// Get statistics as a formatted summary string
    pub fn stats_summary(&self) -> String {
        let s = self.stats.read();
        format!(
            "pools={} vault_rx={} vault_updated={} geyser_updates={}",
            s.pools_tracked,
            s.vault_updates_received,
            s.vault_reserves_updated,
            s.geyser_updates_received
        )
    }

    /// Get pair index statistics - critical for cross-pool arb routing
    /// Shows how many token pairs have multiple pools available
    pub fn pair_index_stats(&self) -> String {
        let index = self.pair_index.read();
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

    /// Get all token pairs that have multiple pools (for pure arb scanning)
    /// Returns: Vec<(base_mint, quote_mint, pool_addresses)>
    pub fn get_multi_pool_pairs(&self) -> Vec<(Pubkey, Pubkey, Vec<Pubkey>)> {
        let index = self.pair_index.read();
        let mut result = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for ((base, quote), pools) in index.iter() {
            if pools.len() < 2 {
                continue;
            }
            
            // Avoid duplicates (we index both directions)
            let key = if base < quote { (*base, *quote) } else { (*quote, *base) };
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);

            result.push((*base, *quote, pools.clone()));
        }

        result
    }
}

impl Default for PoolStateManager {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_pool(address: Pubkey, base: u64, quote: u64) -> PoolState {
        PoolState::new(
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
    fn test_reserve_update() {
        let pool_addr = Pubkey::new_unique();
        let mut pool = make_test_pool(pool_addr, 1_000_000_000, 1_000_000);

        assert_eq!(pool.confirmed_base_reserve, 1_000_000_000);
        assert_eq!(pool.confirmed_quote_reserve, 1_000_000);

        // Update reserves
        pool.update_confirmed_reserves(1_100_000_000, 900_000, 102);
        assert_eq!(pool.confirmed_base_reserve, 1_100_000_000);
        assert_eq!(pool.confirmed_quote_reserve, 900_000);
    }

    #[test]
    fn test_manager_registration() {
        let manager = PoolStateManager::new();

        let pool_addr = Pubkey::new_unique();
        let pool = make_test_pool(pool_addr, 1_000_000_000, 1_000_000);
        manager.register_pool(pool);

        // Get state
        let state = manager.get_pool_state(&pool_addr).unwrap();
        assert_eq!(state.confirmed_base_reserve, 1_000_000_000);

        // Get reserves
        let (base, quote) = manager.get_pool_reserves(&pool_addr).unwrap();
        assert_eq!(base, 1_000_000_000);
        assert_eq!(quote, 1_000_000);
    }

    #[test]
    fn test_spot_price() {
        let pool_addr = Pubkey::new_unique();
        let pool = make_test_pool(pool_addr, 1_000_000_000, 1_000_000);

        // Price = quote/base = 1_000_000 / 1_000_000_000 = 0.001
        let price = pool.spot_price().unwrap();
        assert!((price - 0.001).abs() < 0.0001);
    }

    #[test]
    fn test_virtual_state_creates_simulator() {
        let pool_addr = Pubkey::new_unique();
        let pool = make_test_pool(pool_addr, 1_000_000_000, 1_000_000);

        // Create simulator from virtual state (constant product pools)
        let virtual_state = pool.virtual_state();
        let simulator = PoolSimulator::ConstantProduct(virtual_state);
        assert!(matches!(simulator, PoolSimulator::ConstantProduct(_)));
        
        // Verify swap simulation works
        let (output, _) = simulator.simulate_swap(100_000, true);
        assert!(output > 0);
    }
}