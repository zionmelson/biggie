//! Arbitrage Evaluator
//!
//! Evaluates cross-pool arbitrage opportunities when pool state changes.
//!
//! # How It Works
//!
//! When pool state is updated (via Turbine or Geyser), we:
//! 1. Get all pools for the affected token pair
//! 2. Find the cheapest buy pool and best sell pool
//! 3. Calculate spread vs costs (flash loan fee, slippage, tip)
//! 4. If profitable, build an arb opportunity
//!
//! # Two Evaluation Modes
//!
//! 1. **Swap-Triggered** (via Turbine/Shreds):
//!    - We saw a swap execute in a block
//!    - That swap moved the pool price
//!    - Check if this creates divergence with other pools
//!
//! 2. **State-Triggered** (via Geyser vault updates):
//!    - Vault balance changed (any reason)
//!    - Check if this creates divergence with other pools
//!
//! # Important: No Same-Block Following
//!
//! The `ArbOpportunity` struct includes a `trigger_tx` field for tracking
//! which transaction caused the opportunity, but we do NOT bundle with it.
//! The trigger TX has already executed - our arb goes in the NEXT block
//! as a standalone TX.

use super::parser::{ParsedSwap, TransactionSwaps};
use super::programs::DexType;
use crate::amm_math::{
    find_optimal_borrow_unified, find_optimal_pure_arb_unified,
    simulate_cross_pool_arb_unified, PoolSimulator,
};
use crate::constants::{
    calculate_flash_loan_fee, MIN_PROFIT_LAMPORTS, MIN_SWAP_AMOUNT_LAMPORTS,
    MIN_TRIGGER_SLIPPAGE_BPS,
};
use crate::pool_state::PoolStateManager;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::VersionedTransaction;
use std::sync::Arc;
use std::time::Instant;

/// An arbitrage opportunity triggered by a swap that caused price divergence
#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    /// Unique ID for tracking
    pub id: String,
    /// When this opportunity was detected
    pub detected_at: Instant,
    /// The swap transaction that triggered this opportunity (already executed)
    pub trigger_tx: VersionedTransaction,
    /// The triggering swap details
    pub trigger_swap: ParsedSwap,
    /// Slot where trigger tx was seen
    pub slot: u64,
    /// The pool being traded on
    pub pool: Pubkey,
    /// DEX type for instruction building
    pub dex: DexType,
    /// Token the triggering swap sold (pool got more of this)
    pub trigger_input_mint: Pubkey,
    /// Token the triggering swap bought (pool got less of this)
    pub trigger_output_mint: Pubkey,
    /// Triggering swap's input amount
    pub trigger_amount_in: u64,
    /// Triggering swap's min output (slippage tolerance)
    pub trigger_min_out: u64,
    /// Calculated slippage in bps of the triggering swap
    pub trigger_slippage_bps: u64,
    /// Our arb route: which pools to use for the arbitrage
    pub arb_route: ArbRoute,
    /// Expected profit in lamports
    pub expected_profit_lamports: u64,
    /// Flash loan mint to borrow
    pub borrow_mint: Pubkey,
    /// Amount to borrow for the arb
    pub borrow_amount: u64,
}

/// A pure arbitrage opportunity (no trigger transaction - triggered by price drift)
#[derive(Debug, Clone)]
pub struct PureArbOpportunity {
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
    /// The token pair being arbitraged
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    /// Flash loan mint (what we borrow)
    pub borrow_mint: Pubkey,
    /// Amount to trade
    pub trade_amount: u64,
    /// Expected output from leg 1 (buy)
    pub leg1_output: u64,
    /// Expected output from leg 2 (sell)
    pub leg2_output: u64,
    /// Expected profit in lamports
    pub expected_profit_lamports: u64,
    /// Pool state for buy instruction
    pub buy_pool_state: PoolSwapState,
    /// Pool state for sell instruction
    pub sell_pool_state: PoolSwapState,
    /// Direction: true = buy base on buy_pool, sell on sell_pool
    pub direction: bool,
}

/// The arbitrage route for cross-pool arb
#[derive(Debug, Clone)]
pub struct ArbRoute {
    /// Leg 1: Buy the token that became cheaper after trigger's swap
    pub leg1: ArbLeg,
    /// Leg 2: Sell back for anchor token on another pool
    pub leg2: ArbLeg,
    /// Leg 3: Optional third leg for 3-hop arbs (meme-pair routing through anchor)
    pub leg3: Option<ArbLeg>,
    /// Optimal borrow amount calculated via simulation
    pub optimal_borrow: u64,
    /// Expected profit from this route
    pub expected_profit: u64,
    /// Is this a 3-hop route through an anchor mint?
    pub is_three_hop: bool,
}

/// A single leg of the arbitrage with all data needed to build a swap instruction
#[derive(Debug, Clone)]
pub struct ArbLeg {
    pub pool: Pubkey,
    pub dex: DexType,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    /// Estimated output after this leg
    pub estimated_output: u64,
    /// Pool state data needed for swap instruction building
    pub pool_state: PoolSwapState,
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
}

impl Default for PoolSwapState {
    fn default() -> Self {
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
        }
    }
}

/// Arb Evaluator - processes swaps and finds profitable opportunities
pub struct ArbEvaluator {
    /// Pool state manager - THE SINGLE SOURCE OF TRUTH for pool state
    /// Contains confirmed state (from Geyser) + pending swaps (from Turbine)
    /// Also contains pool metadata and route index
    pool_state: Arc<PoolStateManager>,
    /// Anchor mints we can flash loan
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
    pub swaps_evaluated: std::sync::atomic::AtomicU64,
    pub swaps_too_small: std::sync::atomic::AtomicU64,
    pub swaps_no_pool_info: std::sync::atomic::AtomicU64,
    pub swaps_no_slippage: std::sync::atomic::AtomicU64,
    pub swaps_low_slippage: std::sync::atomic::AtomicU64,
    pub swaps_no_arb_route: std::sync::atomic::AtomicU64,
    pub opportunities_found: std::sync::atomic::AtomicU64,
    pub opportunities_below_threshold: std::sync::atomic::AtomicU64,
    pub three_hop_routes_found: std::sync::atomic::AtomicU64,
    pub pure_arb_scans: std::sync::atomic::AtomicU64,
    pub pure_arb_opportunities: std::sync::atomic::AtomicU64,
}

impl Default for EvaluatorStats {
    fn default() -> Self {
        Self {
            swaps_evaluated: std::sync::atomic::AtomicU64::new(0),
            swaps_too_small: std::sync::atomic::AtomicU64::new(0),
            swaps_no_pool_info: std::sync::atomic::AtomicU64::new(0),
            swaps_no_slippage: std::sync::atomic::AtomicU64::new(0),
            swaps_low_slippage: std::sync::atomic::AtomicU64::new(0),
            swaps_no_arb_route: std::sync::atomic::AtomicU64::new(0),
            opportunities_found: std::sync::atomic::AtomicU64::new(0),
            opportunities_below_threshold: std::sync::atomic::AtomicU64::new(0),
            three_hop_routes_found: std::sync::atomic::AtomicU64::new(0),
            pure_arb_scans: std::sync::atomic::AtomicU64::new(0),
            pure_arb_opportunities: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl ArbEvaluator {
    /// Create a new ArbEvaluator
    ///
    /// The PoolStateManager is THE source of truth for all pool state.
    /// It contains confirmed reserves (from Geyser) plus pending swaps (from ShredStream).
    ///
    /// `anchor_mints` should be the set of mints for which flash loans are configured.
    /// This is typically derived from `flash_loan_reserves.keys()` at startup.
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

    /// Get a PoolSimulator for the given pool using PREDICTED state
    ///
    /// This uses curve-specific math (CLMM, DLMM, or constant product)
    /// and includes pending swaps detected via ShredStream.
    fn get_pool_simulator(&self, pool: &Pubkey) -> Option<PoolSimulator> {
        self.pool_state.get_predicted_simulator(pool)
    }

    /// Get pool reserves (base, quote) using PREDICTED state
    fn get_pool_reserves(&self, pool: &Pubkey) -> Option<(u64, u64)> {
        self.pool_state.get_pool_reserves(pool)
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
            "evaluated={} too_small={} no_pool={} no_slip={} low_slip={} no_route={} found={} below_thresh={}",
            self.stats.swaps_evaluated.load(Ordering::Relaxed),
            self.stats.swaps_too_small.load(Ordering::Relaxed),
            self.stats.swaps_no_pool_info.load(Ordering::Relaxed),
            self.stats.swaps_no_slippage.load(Ordering::Relaxed),
            self.stats.swaps_low_slippage.load(Ordering::Relaxed),
            self.stats.swaps_no_arb_route.load(Ordering::Relaxed),
            self.stats.opportunities_found.load(Ordering::Relaxed),
            self.stats
                .opportunities_below_threshold
                .load(Ordering::Relaxed)
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

    /// Evaluate a transaction's swaps for arbitrage opportunities
    /// Uses atomic stats for lock-free operation (can be called with &self)
    pub fn evaluate(&self, tx_swaps: &TransactionSwaps) -> Vec<ArbOpportunity> {
        use std::sync::atomic::Ordering;
        let mut opportunities = Vec::new();

        for swap in &tx_swaps.swaps {
            self.stats.swaps_evaluated.fetch_add(1, Ordering::Relaxed);

            // Filter 1: Minimum amount
            if let Some(amount) = swap.amount_in {
                if amount < MIN_SWAP_AMOUNT_LAMPORTS {
                    self.stats.swaps_too_small.fetch_add(1, Ordering::Relaxed);
                    tracing::trace!(
                        pool = %swap.pool,
                        amount = amount,
                        min_required = MIN_SWAP_AMOUNT_LAMPORTS,
                        "REJECT: swap amount too small"
                    );
                    continue;
                }
            } else {
                tracing::trace!(
                    pool = %swap.pool,
                    "REJECT: no amount_in on swap"
                );
                continue; // No amount = can't evaluate
            }

            // Filter 2: Need pool info for arb route (query PSM)
            let pool_info = match self.get_pool_info(&swap.pool) {
                Some(info) => info,
                None => {
                    self.stats
                        .swaps_no_pool_info
                        .fetch_add(1, Ordering::Relaxed);
                    // TRACE level: these are expected for memecoins we don't track
                    tracing::trace!(
                        pool = %swap.pool,
                        dex = ?swap.dex,
                        "unknown pool (not tracked)"
                    );
                    continue;
                }
            };

            // Filter 3: Calculate triggering swap's slippage
            // Try to calculate slippage from reserves; fall back to min_out ratio if reserves unavailable
            let (trigger_slippage_bps, trigger_amount_in, trigger_min_out) =
                match self.calculate_trigger_slippage(swap, &pool_info) {
                    Some(s) => s,
                    None => {
                        // Fallback: estimate slippage from min_out
                        // If min_out is very low relative to input, assume high slippage tolerance
                        if let (Some(amount_in), Some(min_out)) = (swap.amount_in, swap.min_amount_out) {
                            // If min_out is 0-1, treat as unlimited slippage (common for memecoins)
                            let estimated_slippage = if min_out <= 1 {
                                10000 // 100% slippage tolerance
                            } else {
                                // Rough estimate: ratio suggests expected output, slippage is buffer
                                // This is imprecise but allows us to proceed to arb route check
                                500 // Default 5% assumed slippage
                            };
                            tracing::trace!(
                                pool = %swap.pool,
                                amount_in = amount_in,
                                min_out = min_out,
                                estimated_slippage = estimated_slippage,
                                "Using estimated slippage (reserves unavailable)"
                            );
                            (estimated_slippage, amount_in, min_out)
                        } else {
                            self.stats.swaps_no_slippage.fetch_add(1, Ordering::Relaxed);
                            tracing::debug!(
                                pool = %swap.pool,
                                amount_in = ?swap.amount_in,
                                min_out = ?swap.min_amount_out,
                                "REJECT: could not calculate slippage (missing amounts)"
                            );
                            continue;
                        }
                    }
                };

            if trigger_slippage_bps < MIN_TRIGGER_SLIPPAGE_BPS {
                self.stats.swaps_low_slippage.fetch_add(1, Ordering::Relaxed);
                tracing::trace!(
                    pool = %swap.pool,
                    slippage_bps = trigger_slippage_bps,
                    min_required = MIN_TRIGGER_SLIPPAGE_BPS,
                    "REJECT: trigger slippage too low"
                );
                continue; // Not enough slippage to extract profit
            }

            // Filter 4: Find arb route (need a second pool for the pair)
            let (trigger_input, trigger_output) = self.infer_swap_mints(swap, &pool_info);

            let arb_route = match self.find_arb_route(
                &swap.pool,
                &trigger_input,
                &trigger_output,
                trigger_amount_in,
            ) {
                Some(route) => route,
                None => {
                    self.stats
                        .swaps_no_arb_route
                        .fetch_add(1, Ordering::Relaxed);
                    // TRACE level: single-pool pairs can't be arbed, this is expected
                    tracing::trace!(
                        pool = %swap.pool,
                        input = %trigger_input,
                        output = %trigger_output,
                        amount = trigger_amount_in,
                        "no alternate pool for pair"
                    );
                    continue;
                }
            };

            // The arb route now contains the optimal borrow amount calculated via simulation
            let borrow_mint = arb_route.leg1.input_mint;
            let borrow_amount = arb_route.optimal_borrow;

            // Filter 5: Apply slippage buffer to expected profit
            // The simulation is perfect but real execution has slippage on both legs
            // Total slippage: ~3% on leg1 + ~3% on leg2 = ~6% worst case
            // We require expected_profit to survive 10% slippage to be safe
            const ARB_SLIPPAGE_BUFFER_BPS: u64 = 1000; // 10%
            let expected_profit = arb_route
                .expected_profit
                .saturating_mul(10000 - ARB_SLIPPAGE_BUFFER_BPS)
                / 10000;

            if expected_profit < MIN_PROFIT_LAMPORTS {
                self.stats
                    .opportunities_below_threshold
                    .fetch_add(1, Ordering::Relaxed);
                // Log near-misses for diagnostics (raw profit before slippage buffer)
                tracing::debug!(
                    "NEAR-MISS: raw_profit={} adjusted={} min={} pool={} trigger_amt={}",
                    arb_route.expected_profit,
                    expected_profit,
                    MIN_PROFIT_LAMPORTS,
                    swap.pool,
                    trigger_amount_in
                );
                continue;
            }

            // CRITICAL: Verify leg2 output after slippage covers flash loan repay
            // Flash loan repay = borrow_amount + 0.3% fee
            let flash_loan_repay =
                borrow_amount.saturating_add(calculate_flash_loan_fee(borrow_amount));
            let leg2_after_slippage =
                arb_route.leg2.estimated_output.saturating_mul(10000 - 300) / 10000; // 3% slippage on leg2

            if leg2_after_slippage < flash_loan_repay {
                tracing::debug!(
                    "Arb rejected: leg2 after slippage {} < flash repay {}",
                    leg2_after_slippage,
                    flash_loan_repay
                );
                self.stats
                    .opportunities_below_threshold
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }

            tracing::info!(
                "OPPORTUNITY FOUND: profit={} lamports ({:.6} SOL), borrow={} lamports",
                expected_profit,
                expected_profit as f64 / 1e9,
                borrow_amount
            );

            // Create opportunity
            let opp = ArbOpportunity {
                id: format!("{:x}", rand::random::<u64>()),
                detected_at: tx_swaps.received_at,
                trigger_tx: tx_swaps.transaction.clone(),
                trigger_swap: swap.clone(),
                slot: tx_swaps.slot,
                pool: swap.pool,
                dex: swap.dex,
                trigger_input_mint: trigger_input,
                trigger_output_mint: trigger_output,
                trigger_amount_in,
                trigger_min_out,
                trigger_slippage_bps,
                arb_route,
                expected_profit_lamports: expected_profit, // Use slippage-adjusted profit
                borrow_mint,
                borrow_amount,
            };

            self.stats
                .opportunities_found
                .fetch_add(1, Ordering::Relaxed);
            opportunities.push(opp);
        }

        opportunities
    }

    /// Calculate the triggering swap's effective slippage from their swap params
    /// Uses predicted state if available for more accurate pricing
    fn calculate_trigger_slippage(
        &self,
        swap: &ParsedSwap,
        pool_info: &PoolInfo,
    ) -> Option<(u64, u64, u64)> {
        let amount_in = swap.amount_in?;
        let min_out = swap.min_amount_out?;

        // Get current pool reserves using PREDICTED STATE
        // This accounts for pending swaps that haven't confirmed yet
        let (input_reserve, output_reserve) = {
            let (base_reserve, quote_reserve) = self.get_pool_reserves(&swap.pool)?;

            if swap.input_mint == Some(pool_info.base_mint) {
                (base_reserve, quote_reserve)
            } else {
                (quote_reserve, base_reserve)
            }
        };

        if input_reserve == 0 || output_reserve == 0 {
            return None;
        }

        // Constant product expected output (ignoring fees for simplicity)
        let expected_out = (amount_in as u128 * output_reserve as u128
            / (input_reserve as u128 + amount_in as u128)) as u64;

        if expected_out == 0 {
            return None;
        }

        // Slippage = (expected - min) / expected * 10000
        let slippage_bps = if expected_out > min_out {
            ((expected_out - min_out) as u128 * 10000 / expected_out as u128) as u64
        } else {
            0
        };

        Some((slippage_bps, amount_in, min_out))
    }

    /// Infer input/output mints from swap and pool info
    fn infer_swap_mints(&self, swap: &ParsedSwap, pool_info: &PoolInfo) -> (Pubkey, Pubkey) {
        if let (Some(input), Some(output)) = (swap.input_mint, swap.output_mint) {
            (input, output)
        } else {
            // Infer from direction
            match swap.direction {
                Some(super::parser::SwapDirection::AtoB) => {
                    (pool_info.base_mint, pool_info.quote_mint)
                }
                Some(super::parser::SwapDirection::BtoA) => {
                    (pool_info.quote_mint, pool_info.base_mint)
                }
                None => {
                    // Default assumption: base -> quote
                    (pool_info.base_mint, pool_info.quote_mint)
                }
            }
        }
    }

    /// Find an arbitrage route for the opportunity
    ///
    /// CRITICAL: Single-pool arb is IMPOSSIBLE for post-execution transactions.
    /// When a swap pushes A->B, they push price making B expensive on this pool.
    /// We can only profit with cross-pool arb:
    ///   1. Buy B cheap on Pool Y (unaffected by trigger)
    ///   2. Sell B expensive on Pool X (post-trigger)
    ///
    /// NOTE: 3-hop routing (through anchor for meme pairs) is disabled for simplicity.
    /// Only 2-hop routes are evaluated.
    fn find_arb_route(
        &self,
        trigger_pool: &Pubkey,
        trigger_input: &Pubkey,
        trigger_output: &Pubkey,
        trigger_amount: u64,
    ) -> Option<ArbRoute> {
        // The triggering swap sold input and bought output on trigger_pool
        // After their swap, output is expensive on trigger_pool
        //
        // Cross-pool arb strategy:
        // 1. Find alternate pool trading the same pair
        // 2. Buy output (expensive on trigger_pool) cheap on alt_pool
        // 3. Sell output on trigger_pool (post-trigger) for profit

        // Check if either token is an anchor (required for 2-hop)
        let has_anchor_input = self.anchor_mints.contains(trigger_input);
        let has_anchor_output = self.anchor_mints.contains(trigger_output);

        if has_anchor_input || has_anchor_output {
            // 2-hop arb: one of the tokens is an anchor
            self.find_2hop_arb_route(trigger_pool, trigger_input, trigger_output, trigger_amount)
        } else {
            // 3-hop disabled - skip meme pairs without anchor
            tracing::trace!(
                pool = %trigger_pool,
                input = %trigger_input,
                output = %trigger_output,
                "Skipping meme pair - 3-hop routing disabled"
            );
            None
        }
    }

    /// Standard 2-hop cross-pool arbitrage (when one token is an anchor)
    /// Now evaluates ALL alternate pools and picks the most profitable route
    fn find_2hop_arb_route(
        &self,
        trigger_pool: &Pubkey,
        trigger_input: &Pubkey,
        trigger_output: &Pubkey,
        trigger_amount: u64,
    ) -> Option<ArbRoute> {
        let borrow_mint = if self.anchor_mints.contains(trigger_input) {
            *trigger_input
        } else {
            *trigger_output
        };

        // Find ALL alternate pools and evaluate each one
        let alt_pools = self.pool_state.find_all_alternate_pools(
            trigger_pool,
            trigger_input,
            trigger_output,
        );

        let num_alt_pools = alt_pools.len();
        if num_alt_pools == 0 {
            tracing::trace!(
                trigger_pool = %trigger_pool,
                input = %trigger_input,
                output = %trigger_output,
                "No alternate pool found for cross-pool arb"
            );
            return None;
        }

        // Get trigger pool info and simulator (shared across all evaluations)
        let trigger_pool_info = self.get_pool_info(trigger_pool)?;
        let (trigger_base_mint, _) = self.pool_state.get_pool_mints(trigger_pool)?;
        let trigger_base_to_quote = *trigger_input == trigger_base_mint;
        let pool_x = self.get_pool_simulator(trigger_pool)?;
        let max_borrow = trigger_amount * 2;

        // Track best route across all alternate pools
        let mut best_route: Option<(crate::pool_state::PoolState, u64, u64, bool)> = None;

        for alt_pool_state in alt_pools {
            let alt_base_mint = alt_pool_state.base_mint;
            let borrow_is_base_on_y = borrow_mint == alt_base_mint;
            let pool_y = alt_pool_state.predicted_simulator();

            // Find optimal borrow for this specific alternate pool
            let (optimal_borrow, expected_profit) = find_optimal_borrow_unified(
                &pool_x,
                &pool_y,
                trigger_amount,
                trigger_base_to_quote,
                borrow_is_base_on_y,
                max_borrow,
                30, // 0.3% flash loan fee
            );

            // Track if this is better than our current best
            if expected_profit > 0 {
                let is_better = match &best_route {
                    None => true,
                    Some((_, _, best_profit, _)) => expected_profit > *best_profit,
                };
                if is_better {
                    best_route = Some((alt_pool_state, optimal_borrow, expected_profit, borrow_is_base_on_y));
                }
            }
        }

        // Use the best route found
        let (alt_pool_state, optimal_borrow, expected_profit, borrow_is_base_on_y) = best_route?;

        tracing::info!(
            trigger_pool = %trigger_pool,
            alt_pool = %alt_pool_state.address,
            trigger_amount = trigger_amount,
            optimal_borrow = optimal_borrow,
            expected_profit = expected_profit,
            pools_evaluated = num_alt_pools,
            "ARB_SIM: 2-hop best route selected from {} pools", num_alt_pools
        );

        // Get fresh simulator for the best pool to compute final simulation
        let pool_y = alt_pool_state.predicted_simulator();

        // Simulate the arb to get leg outputs (using unified curve-aware simulation)
        let simulation = simulate_cross_pool_arb_unified(
            &pool_x,
            &pool_y,
            trigger_amount,
            trigger_base_to_quote,
            optimal_borrow,
            borrow_is_base_on_y,
            30,
        )?;

        // Build pool swap states for instruction building (from PSM pool state)
        let leg1_pool_state = self.build_pool_swap_state_from_psm(&alt_pool_state);
        let leg2_pool_state = self.build_pool_swap_state(trigger_pool)?;

        // Leg 1: swap on pool Y (unaffected by trigger)
        // Leg 2: swap on pool X (post-trigger, profitable direction)
        let (leg1_input, leg1_output_mint) = if borrow_is_base_on_y {
            (borrow_mint, *trigger_output)
        } else {
            (borrow_mint, *trigger_input)
        };

        let (leg2_input, leg2_output_mint) = (leg1_output_mint, borrow_mint);

        Some(ArbRoute {
            leg1: ArbLeg {
                pool: alt_pool_state.address,
                dex: alt_pool_state.dex,
                input_mint: leg1_input,
                output_mint: leg1_output_mint,
                estimated_output: simulation.leg1_output,
                pool_state: leg1_pool_state,
            },
            leg2: ArbLeg {
                pool: *trigger_pool,
                dex: trigger_pool_info.dex,
                input_mint: leg2_input,
                output_mint: leg2_output_mint,
                estimated_output: simulation.leg2_output,
                pool_state: leg2_pool_state,
            },
            leg3: None,
            optimal_borrow,
            expected_profit,
            is_three_hop: false,
        })
    }

    // NOTE: 3-hop routing removed for simplicity. If needed in the future,
    // the route goes: ANCHOR → token_a (Pool A) → token_b (trigger pool) → ANCHOR (Pool B)

    /// Build PoolSwapState directly from a PoolState (from PSM)
    fn build_pool_swap_state_from_psm(
        &self,
        state: &crate::pool_state::PoolState,
    ) -> PoolSwapState {
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
        }
    }

    /// Build PoolSwapState from PSM by pool address
    fn build_pool_swap_state(&self, pool: &Pubkey) -> Option<PoolSwapState> {
        // Get full pool state from PSM (single source of truth)
        if let Some(state) = self.pool_state.get_predicted_state(pool) {
            if state.has_metadata() {
                return Some(self.build_pool_swap_state_from_psm(&state));
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
        })
    }

    // =========================================================================
    // PURE ARBITRAGE (no trigger transaction)
    // =========================================================================

    /// Scan for pure arbitrage opportunities on a specific pool that just had a price update.
    ///
    /// Called when Geyser reports a vault balance change. Checks if the price
    /// discrepancy between this pool and others trading the same pair is profitable.
    ///
    /// Returns a pure arb opportunity if profitable, None otherwise.
    pub fn scan_pure_arb_for_pool(&self, updated_pool: &Pubkey) -> Option<PureArbOpportunity> {
        use std::sync::atomic::Ordering;
        self.stats.pure_arb_scans.fetch_add(1, Ordering::Relaxed);

        // Get pool info
        let pool_info = self.get_pool_info(updated_pool)?;
        
        // Only scan pairs where at least one token is an anchor (for flash loans)
        let borrow_mint = if self.anchor_mints.contains(&pool_info.base_mint) {
            pool_info.base_mint
        } else if self.anchor_mints.contains(&pool_info.quote_mint) {
            pool_info.quote_mint
        } else {
            // Neither token is an anchor - can't flash loan
            return None;
        };

        // Find alternate pools for this pair
        let alt_pools = self.pool_state.get_pools_for_pair(
            &pool_info.base_mint,
            &pool_info.quote_mint,
        );

        if alt_pools.len() < 2 {
            // Need at least 2 pools for cross-pool arb
            return None;
        }

        // Get simulator for updated pool
        let pool_a_sim = self.get_pool_simulator(updated_pool)?;
        let pool_a_state = self.pool_state.get_predicted_state(updated_pool)?;

        // Check against each alternate pool
        for alt_pool_addr in &alt_pools {
            if alt_pool_addr == updated_pool {
                continue;
            }

            let pool_b_sim = match self.get_pool_simulator(alt_pool_addr) {
                Some(sim) => sim,
                None => continue,
            };
            let pool_b_state = match self.pool_state.get_predicted_state(alt_pool_addr) {
                Some(state) => state,
                None => continue,
            };

            // Determine max trade size (use smaller pool's liquidity)
            let max_trade = std::cmp::min(
                pool_a_state.confirmed_base_reserve / 10, // Max 10% of pool
                pool_b_state.confirmed_base_reserve / 10,
            );

            if max_trade < MIN_SWAP_AMOUNT_LAMPORTS {
                continue;
            }

            // Find optimal pure arb
            let arb_result = find_optimal_pure_arb_unified(
                &pool_a_sim,
                &pool_b_sim,
                max_trade,
                30, // 0.3% flash loan fee
            );

            if let Some(result) = arb_result {
                // Apply slippage buffer
                let adjusted_profit = result.profit.saturating_mul(9000) / 10000; // 10% buffer
                
                if adjusted_profit >= MIN_PROFIT_LAMPORTS {
                    self.stats.pure_arb_opportunities.fetch_add(1, Ordering::Relaxed);

                    tracing::info!(
                        pool_a = %updated_pool,
                        pool_b = %alt_pool_addr,
                        profit = adjusted_profit,
                        trade_amount = result.trade_amount,
                        "PURE ARB OPPORTUNITY FOUND"
                    );

                    // Determine which pool is buy vs sell based on direction
                    let (buy_pool, sell_pool, buy_state, sell_state, buy_dex, sell_dex) = 
                        if result.direction {
                            (*updated_pool, *alt_pool_addr, 
                             self.build_pool_swap_state_from_psm(&pool_a_state),
                             self.build_pool_swap_state_from_psm(&pool_b_state),
                             pool_info.dex, pool_b_state.dex)
                        } else {
                            (*alt_pool_addr, *updated_pool,
                             self.build_pool_swap_state_from_psm(&pool_b_state),
                             self.build_pool_swap_state_from_psm(&pool_a_state),
                             pool_b_state.dex, pool_info.dex)
                        };

                    return Some(PureArbOpportunity {
                        id: format!("pure_{:x}", rand::random::<u64>()),
                        detected_at: Instant::now(),
                        buy_pool,
                        sell_pool,
                        buy_dex,
                        sell_dex,
                        base_mint: pool_info.base_mint,
                        quote_mint: pool_info.quote_mint,
                        borrow_mint,
                        trade_amount: result.trade_amount,
                        leg1_output: result.leg1_output,
                        leg2_output: result.leg2_output,
                        expected_profit_lamports: adjusted_profit,
                        buy_pool_state: buy_state,
                        sell_pool_state: sell_state,
                        direction: result.direction,
                    });
                }
            }
        }

        None
    }

    /// Scan all multi-pool pairs for pure arbitrage opportunities.
    ///
    /// This is a more expensive operation - should be called periodically
    /// rather than on every update.
    pub fn scan_all_pure_arb(&self) -> Vec<PureArbOpportunity> {
        let mut opportunities = Vec::new();
        
        // Get all pools and check each one
        // This is O(n) where n = number of pools, but the inner loop
        // exits early for single-pool pairs
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
            if let Some(opp) = self.scan_pure_arb_for_pool(&pools[0]) {
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
        // Test with SOL and USDC as anchor mints
        let anchor_mints = vec![WSOL_MINT, USDC_MINT];
        let evaluator = ArbEvaluator::new(pool_state, anchor_mints);
        assert_eq!(evaluator.anchor_mints.len(), 2);
    }
}
