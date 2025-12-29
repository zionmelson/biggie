//! Pure Arbitrage Evaluator
//!
//! Evaluates cross-pool arbitrage opportunities based on price discrepancies.
//!
//! # How It Works
//!
//! When pool state is updated (via Geyser vault updates), we:
//! 1. Get all pools for the affected token pair
//! 2. Find the cheapest buy pool and best sell pool
//! 3. Calculate spread vs costs (flash loan fee, slippage, tip)
//! 4. If profitable, build an arb opportunity
//!
//! # Pure Arbitrage Only
//!
//! This evaluator focuses on price divergence between pools - we don't
//! track or follow individual swap transactions. Price discrepancies can
//! arise from any cause (large trades, liquidity changes, market drift).

use super::programs::DexType;
use crate::amm_math::{find_optimal_pure_arb_unified_with_fees, PoolSimulator};
use crate::constants::{MIN_PROFIT_LAMPORTS, MIN_TRADE_LAMPORTS, DEFAULT_SLIPPAGE_BPS};
use crate::pool_state::PoolStateManager;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use std::time::Instant;

// Re-export from central constants module
/// Minimum trade size in base units (lamports for SOL-based arbs)
/// Note: All amounts in this module are in "base units" - the smallest denomination
/// of each token (e.g., lamports for SOL with 9 decimals, micro-units for USDC with 6 decimals).
/// The simulation handles decimals implicitly through reserve ratios.
#[allow(dead_code)]
const MIN_TRADE: u64 = MIN_TRADE_LAMPORTS;

/// Default slippage tolerance in basis points for swap simulation
/// This is the ONLY slippage buffer applied - no additional multipliers.
/// Cast to u16 for compatibility with simulation functions.
const SLIPPAGE_BPS: u16 = DEFAULT_SLIPPAGE_BPS as u16;

/// A pure arbitrage opportunity detected from price divergence between pools
#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    /// Unique ID for tracking
    pub id: String,
    /// When this opportunity was detected
    pub detected_at: Instant,
    /// Pool to buy on (cheaper price)
    pub buy_pool: Pubkey,
    /// Pool to sell on (more expensive)
    pub sell_pool: Pubkey,
    /// DEX type for buy pool
    pub buy_dex: DexType,
    /// DEX type for sell pool
    pub sell_dex: DexType,
    /// The token pair being arbitraged (base = non-anchor token)
    pub base_mint: Pubkey,
    /// Quote mint (typically the anchor token we flash loan)
    pub quote_mint: Pubkey,
    /// Flash loan mint (what we borrow - always an anchor token, in base units)
    pub borrow_mint: Pubkey,
    /// Amount to trade/borrow (in borrow token base units - e.g., lamports for SOL)
    pub trade_amount: u64,
    /// Expected output from leg 1 (in intermediate token base units)
    pub leg1_output: u64,
    /// Expected output from leg 2 (in borrow token base units)
    pub leg2_output: u64,
    /// Minimum acceptable leg1 output (with slippage, in intermediate token base units)
    pub min_leg1_output: u64,
    /// Minimum acceptable leg2 output (with slippage, in borrow token base units)
    pub min_leg2_output: u64,
    /// Expected profit in borrow token base units (after flash loan fee and slippage buffer)
    pub expected_profit_lamports: u64,
    /// Pool state for buy instruction
    pub buy_pool_state: PoolSwapState,
    /// Pool state for sell instruction
    pub sell_pool_state: PoolSwapState,
    /// Direction: true = buy base on buy_pool, sell on sell_pool
    pub direction: bool,
}

/// Pool state data needed to build swap instructions for different DEX types
#[derive(Debug, Clone)]
pub struct PoolSwapState {
    // === Common fields ===
    /// Token A/Base/X mint (order varies by DEX)
    pub token_mint_a: Pubkey,
    /// Token B/Quote/Y mint
    pub token_mint_b: Pubkey,
    /// Token A vault
    pub token_vault_a: Pubkey,
    /// Token B vault
    pub token_vault_b: Pubkey,

    // === Raydium AMM specific ===
    /// AMM authority PDA (Raydium AMM)
    pub amm_authority: Option<Pubkey>,

    // === CLMM common (Raydium CLMM, Orca Whirlpool, PancakeSwap) ===
    /// AMM config account (Raydium/PancakeSwap CLMM)
    pub amm_config: Option<Pubkey>,
    /// Observation/Oracle state (all CLMM)
    pub observation_state: Option<Pubkey>,
    /// Current tick (CLMM) or bin_id (DLMM)
    pub current_tick_or_bin: i32,
    /// Tick spacing (CLMM) or bin_step (DLMM)
    pub spacing: u16,
    /// Tick arrays for the swap (Orca needs 3, Raydium needs 1+)
    pub tick_arrays: Vec<Pubkey>,

    // === Meteora DLMM specific ===
    /// Bin array bitmap extension (DLMM)
    pub bin_array_bitmap_extension: Option<Pubkey>,
    /// Bin arrays for the swap (DLMM)
    pub bin_arrays: Vec<Pubkey>,

    // === Meteora DAMM specific ===
    /// Pool authority PDA (DAMM)
    pub pool_authority: Option<Pubkey>,

    // === Token-2022 support ===
    /// Token program for token A (SPL Token or Token-2022)
    pub token_program_a: Pubkey,
    /// Token program for token B (SPL Token or Token-2022)
    pub token_program_b: Pubkey,
    /// Transfer fee bps for token A (Token-2022 only)
    pub transfer_fee_bps_a: u16,
    /// Transfer fee bps for token B (Token-2022 only)
    pub transfer_fee_bps_b: u16,
}

impl Default for PoolSwapState {
    fn default() -> Self {
        use crate::constants::TOKEN_PROGRAM;
        Self {
            token_mint_a: Pubkey::default(),
            token_mint_b: Pubkey::default(),
            token_vault_a: Pubkey::default(),
            token_vault_b: Pubkey::default(),
            amm_authority: None,
            amm_config: None,
            observation_state: None,
            current_tick_or_bin: 0,
            spacing: 0,
            tick_arrays: Vec::new(),
            bin_array_bitmap_extension: None,
            bin_arrays: Vec::new(),
            pool_authority: None,
            token_program_a: TOKEN_PROGRAM,
            token_program_b: TOKEN_PROGRAM,
            transfer_fee_bps_a: 0,
            transfer_fee_bps_b: 0,
        }
    }
}

/// Arb Evaluator - scans for pure arbitrage opportunities across pools
pub struct ArbEvaluator {
    /// Pool state manager - THE SINGLE SOURCE OF TRUTH for pool state
    pool_state: Arc<PoolStateManager>,
    /// Anchor mints we can flash loan (SOL, USDC, etc.)
    anchor_mints: Vec<Pubkey>,
    /// Statistics
    pub stats: EvaluatorStats,
}

/// Information about a known pool (metadata only - reserves come from PoolStateManager)
#[derive(Debug, Clone)]
pub struct PoolInfo {
    pub address: Pubkey,
    pub dex: DexType,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
}

/// Statistics for the evaluator (using atomics for lock-free updates)
#[derive(Debug)]
pub struct EvaluatorStats {
    pub pools_scanned: std::sync::atomic::AtomicU64,
    pub pairs_evaluated: std::sync::atomic::AtomicU64,
    pub opportunities_found: std::sync::atomic::AtomicU64,
    pub opportunities_below_threshold: std::sync::atomic::AtomicU64,
}

impl Default for EvaluatorStats {
    fn default() -> Self {
        Self {
            pools_scanned: std::sync::atomic::AtomicU64::new(0),
            pairs_evaluated: std::sync::atomic::AtomicU64::new(0),
            opportunities_found: std::sync::atomic::AtomicU64::new(0),
            opportunities_below_threshold: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl ArbEvaluator {
    /// Create a new ArbEvaluator
    ///
    /// The PoolStateManager is THE source of truth for all pool state.
    /// `anchor_mints` should be the set of mints for which flash loans are configured.
    pub fn new(pool_state: Arc<PoolStateManager>, anchor_mints: Vec<Pubkey>) -> Self {
        tracing::info!(
            "ArbEvaluator initialized with {} anchor mints: {:?}",
            anchor_mints.len(),
            anchor_mints.iter().map(|m| m.to_string()).collect::<Vec<_>>()
        );
        Self {
            pool_state,
            anchor_mints,
            stats: EvaluatorStats::default(),
        }
    }

    /// Get a PoolSimulator for the given pool using confirmed state
    fn get_pool_simulator(&self, pool: &Pubkey) -> Option<PoolSimulator> {
        self.pool_state.get_simulator(pool)
    }

    /// Get pool info from PoolStateManager
    fn get_pool_info(&self, pool: &Pubkey) -> Option<PoolInfo> {
        let (base_mint, quote_mint) = self.pool_state.get_pool_mints(pool)?;
        let dex = self.pool_state.get_pool_dex(pool)?;
        Some(PoolInfo {
            address: *pool,
            dex,
            base_mint,
            quote_mint,
        })
    }

    /// Get stats summary
    pub fn stats_summary(&self) -> String {
        use std::sync::atomic::Ordering;
        format!(
            "scanned={} pairs={} found={} below_thresh={}",
            self.stats.pools_scanned.load(Ordering::Relaxed),
            self.stats.pairs_evaluated.load(Ordering::Relaxed),
            self.stats.opportunities_found.load(Ordering::Relaxed),
            self.stats.opportunities_below_threshold.load(Ordering::Relaxed),
        )
    }

    /// Get number of known pools (from PSM)
    pub fn known_pool_count(&self) -> usize {
        self.pool_state.pool_count()
    }

    /// Get a reference to stats
    pub fn stats(&self) -> &EvaluatorStats {
        &self.stats
    }

    /// Build PoolSwapState from a PoolState
    fn build_pool_swap_state(&self, state: &crate::pool_state::PoolState) -> PoolSwapState {
        PoolSwapState {
            token_mint_a: state.base_mint,
            token_mint_b: state.quote_mint,
            token_vault_a: state.token_vault_a,
            token_vault_b: state.token_vault_b,
            current_tick_or_bin: state.confirmed_tick_or_bin.unwrap_or(0),
            spacing: state.spacing.unwrap_or(0),
            amm_authority: state.amm_authority,
            amm_config: state.amm_config,
            observation_state: state.observation_state,
            tick_arrays: state.tick_arrays.clone(),
            bin_array_bitmap_extension: state.bin_array_bitmap_extension,
            bin_arrays: state.bin_arrays.clone(),
            pool_authority: state.pool_authority,
            token_program_a: state.token_program_a,
            token_program_b: state.token_program_b,
            transfer_fee_bps_a: state.transfer_fee_bps_a,
            transfer_fee_bps_b: state.transfer_fee_bps_b,
        }
    }

    /// Build PoolSwapState from PSM by pool address
    #[allow(dead_code)]
    fn build_pool_swap_state_by_address(&self, pool: &Pubkey) -> Option<PoolSwapState> {
        if let Some(state) = self.pool_state.get_pool_state(pool) {
            if state.has_metadata() {
                return Some(self.build_pool_swap_state(&state));
            }
        }

        // Fallback: minimal state from PSM mints (will likely fail on execution)
        let (base_mint, quote_mint) = self.pool_state.get_pool_mints(pool)?;
        tracing::warn!(
            pool = %pool,
            "Pool missing metadata - swap instruction may fail"
        );
        Some(PoolSwapState {
            token_mint_a: base_mint,
            token_mint_b: quote_mint,
            token_vault_a: Pubkey::default(),
            token_vault_b: Pubkey::default(),
            current_tick_or_bin: 0,
            spacing: 0,
            amm_authority: None,
            amm_config: None,
            observation_state: None,
            tick_arrays: Vec::new(),
            bin_array_bitmap_extension: None,
            bin_arrays: Vec::new(),
            pool_authority: None,
            token_program_a: crate::constants::TOKEN_PROGRAM,
            token_program_b: crate::constants::TOKEN_PROGRAM,
            transfer_fee_bps_a: 0,
            transfer_fee_bps_b: 0,
        })
    }

    /// Scan for arbitrage opportunities on a specific pool that just had a price update.
    ///
    /// Called when Geyser reports a vault balance change. Checks if the price
    /// discrepancy between this pool and others trading the same pair is profitable.
    ///
    /// Returns an arb opportunity if profitable, None otherwise.
    pub fn scan_pool(&self, updated_pool: &Pubkey) -> Option<ArbOpportunity> {
        use std::sync::atomic::Ordering;
        self.stats.pools_scanned.fetch_add(1, Ordering::Relaxed);

        // Get pool info
        let pool_info = self.get_pool_info(updated_pool)?;

        // Only scan pairs where at least one token is an anchor (for flash loans)
        let (borrow_mint, intermediate_mint) = if self.anchor_mints.contains(&pool_info.base_mint) {
            (pool_info.base_mint, pool_info.quote_mint)
        } else if self.anchor_mints.contains(&pool_info.quote_mint) {
            (pool_info.quote_mint, pool_info.base_mint)
        } else {
            // Neither token is an anchor - can't flash loan
            return None;
        };

        // Find alternate pools for this pair
        let alt_pools = self
            .pool_state
            .get_pools_for_pair(&pool_info.base_mint, &pool_info.quote_mint);

        if alt_pools.len() < 2 {
            // Need at least 2 pools for cross-pool arb
            return None;
        }

        self.stats.pairs_evaluated.fetch_add(1, Ordering::Relaxed);

        // Get simulator for updated pool
        let pool_a_sim = self.get_pool_simulator(updated_pool)?;
        let pool_a_state = self.pool_state.get_pool_state(updated_pool)?;

        // SAFEGUARD: Skip if pool A has zero reserves (data not yet populated)
        if pool_a_state.confirmed_base_reserve == 0 || pool_a_state.confirmed_quote_reserve == 0 {
            return None;
        }

        // Check against each alternate pool
        for alt_pool_addr in &alt_pools {
            if alt_pool_addr == updated_pool {
                continue;
            }

            let pool_b_sim = match self.get_pool_simulator(alt_pool_addr) {
                Some(sim) => sim,
                None => continue,
            };
            let pool_b_state = match self.pool_state.get_pool_state(alt_pool_addr) {
                Some(state) => state,
                None => continue,
            };

            // SAFEGUARD: Skip if pool B has zero reserves (data not yet populated)
            if pool_b_state.confirmed_base_reserve == 0 || pool_b_state.confirmed_quote_reserve == 0 {
                continue;
            }

            // LOG: Show spot prices using proper curve-specific pricing
            let price_a = pool_a_state.spot_price().unwrap_or(0.0);
            let price_b = pool_b_state.spot_price().unwrap_or(0.0);
            tracing::debug!(
                pool_a = %updated_pool,
                pool_a_dex = ?pool_a_state.dex,
                pool_a_price = %format!("{:.6}", price_a),
                pool_b = %alt_pool_addr,
                pool_b_dex = ?pool_b_state.dex,
                pool_b_price = %format!("{:.6}", price_b),
                spread_bps = %format!("{:.2}", ((price_a - price_b).abs() / price_a.max(price_b).max(1e-10)) * 10_000.0),
                "POOL PAIR PRICES"
            );

            // Determine if borrow_mint is the base or quote of the pool
            // This affects:
            // 1. Which reserve to use for max_trade (we borrow in borrow_mint units)
            // 2. Swap direction in simulation (must match execution direction)
            let borrow_is_base = borrow_mint == pool_a_state.base_mint;

            // Determine max trade size using the BORROW token's reserve (not always base!)
            // We can only borrow up to ~10% of the pool's liquidity in that token
            let max_trade = if borrow_is_base {
                // borrow_mint is base, so use base reserves
                std::cmp::min(
                    pool_a_state.confirmed_base_reserve / 10,
                    pool_b_state.confirmed_base_reserve / 10,
                )
            } else {
                // borrow_mint is quote, so use quote reserves  
                std::cmp::min(
                    pool_a_state.confirmed_quote_reserve / 10,
                    pool_b_state.confirmed_quote_reserve / 10,
                )
            };

            if max_trade < MIN_TRADE_LAMPORTS {
                continue;
            }

            // Get Token-2022 transfer fees (use pool_a's fees since both pools have same tokens)
            let transfer_fee_base_bps = pool_a_state.transfer_fee_bps_a;
            let transfer_fee_quote_bps = pool_a_state.transfer_fee_bps_b;

            // Find optimal pure arb
            // Pass borrow_is_base so simulation uses correct swap directions
            let arb_result = find_optimal_pure_arb_unified_with_fees(
                &pool_a_sim,
                &pool_b_sim,
                max_trade,
                30,  // flash_fee_bps: 0.3%
                borrow_is_base,
                SLIPPAGE_BPS,
                transfer_fee_base_bps,
                transfer_fee_quote_bps,
            );

            if let Some(result) = arb_result {
                // Use profit directly - slippage is already factored into min_leg1/leg2_output
                // No additional buffer needed (was causing double-counting)
                if result.profit < MIN_PROFIT_LAMPORTS {
                    self.stats
                        .opportunities_below_threshold
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                self.stats
                    .opportunities_found
                    .fetch_add(1, Ordering::Relaxed);

                tracing::info!(
                    pool_a = %updated_pool,
                    pool_a_base = pool_a_state.confirmed_base_reserve,
                    pool_a_quote = pool_a_state.confirmed_quote_reserve,
                    pool_b = %alt_pool_addr,
                    pool_b_base = pool_b_state.confirmed_base_reserve,
                    pool_b_quote = pool_b_state.confirmed_quote_reserve,
                    profit = result.profit,
                    trade_amount = result.trade_amount,
                    leg1_output = result.leg1_output,
                    min_leg1 = result.min_leg1_output,
                    leg2_output = result.leg2_output,
                    min_leg2 = result.min_leg2_output,
                    direction = result.direction,
                    borrow_is_base = borrow_is_base,
                    "ARB OPPORTUNITY FOUND"
                );

                // Determine which pool is buy vs sell based on direction
                let (buy_pool, sell_pool, buy_state, sell_state, buy_dex, sell_dex) =
                    if result.direction {
                        (
                            *updated_pool,
                            *alt_pool_addr,
                            self.build_pool_swap_state(&pool_a_state),
                            self.build_pool_swap_state(&pool_b_state),
                            pool_info.dex,
                            pool_b_state.dex,
                        )
                    } else {
                        (
                            *alt_pool_addr,
                            *updated_pool,
                            self.build_pool_swap_state(&pool_b_state),
                            self.build_pool_swap_state(&pool_a_state),
                            pool_b_state.dex,
                            pool_info.dex,
                        )
                    };

                return Some(ArbOpportunity {
                    id: format!("arb_{:x}", rand::random::<u64>()),
                    detected_at: Instant::now(),
                    buy_pool,
                    sell_pool,
                    buy_dex,
                    sell_dex,
                    base_mint: intermediate_mint,
                    quote_mint: borrow_mint,
                    borrow_mint,
                    trade_amount: result.trade_amount,
                    leg1_output: result.leg1_output,
                    leg2_output: result.leg2_output,
                    min_leg1_output: result.min_leg1_output,
                    min_leg2_output: result.min_leg2_output,
                    expected_profit_lamports: result.profit,
                    buy_pool_state: buy_state,
                    sell_pool_state: sell_state,
                    direction: result.direction,
                });
            }
        }

        None
    }

    /// Scan all multi-pool pairs for arbitrage opportunities.
    ///
    /// This is a more expensive operation - should be called periodically
    /// rather than on every update.
    pub fn scan_all_pairs(&self) -> Vec<ArbOpportunity> {
        let mut opportunities = Vec::new();

        let pair_stats = self.pool_state.get_multi_pool_pairs();

        for (base, quote, pools) in pair_stats {
            if pools.len() < 2 {
                continue;
            }

            // Only check pairs with anchor mints
            if !self.anchor_mints.contains(&base) && !self.anchor_mints.contains(&quote) {
                continue;
            }

            // Check first pool against others
            if let Some(opp) = self.scan_pool(&pools[0]) {
                opportunities.push(opp);
            }
        }

        opportunities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{USDC_MINT, WSOL_MINT};

    fn create_test_pool_state() -> Arc<PoolStateManager> {
        Arc::new(PoolStateManager::new())
    }

    #[test]
    fn test_evaluator_creation() {
        let pool_state = create_test_pool_state();
        let anchor_mints = vec![WSOL_MINT, USDC_MINT];
        let evaluator = ArbEvaluator::new(pool_state, anchor_mints);
        assert_eq!(evaluator.anchor_mints.len(), 2);
    }
}
