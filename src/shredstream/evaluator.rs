//! MEV Evaluator - Filters parsed swaps into profitable opportunities
//!
//! Takes ParsedSwaps from shredstream and evaluates them for MEV extraction:
//! - Backrun: Arb after the victim's swap moves the pool price

use super::parser::{ParsedSwap, TransactionSwaps};
use super::programs::DexType;
use crate::amm_math::{
    find_optimal_borrow_unified, simulate_cross_pool_backrun_unified, PoolSimulator,
};
use crate::constants::{
    calculate_flash_loan_fee, MIN_PROFIT_LAMPORTS, MIN_SWAP_AMOUNT_LAMPORTS,
    MIN_VICTIM_SLIPPAGE_BPS,
};
use crate::predictive_state::PredictiveStateManager;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::VersionedTransaction;
use std::sync::Arc;
use std::time::Instant;

/// A backrun MEV opportunity
#[derive(Debug, Clone)]
pub struct MevOpportunity {
    /// Unique ID for tracking
    pub id: String,
    /// When this opportunity was detected
    pub detected_at: Instant,
    /// The victim transaction we're backrunning
    pub victim_tx: VersionedTransaction,
    /// The victim's swap details
    pub victim_swap: ParsedSwap,
    /// Slot where victim tx was seen
    pub slot: u64,
    /// The pool being traded on
    pub pool: Pubkey,
    /// DEX type for instruction building
    pub dex: DexType,
    /// Token the victim is selling (pool gets more of this)
    pub victim_input_mint: Pubkey,
    /// Token the victim is buying (pool gets less of this)
    pub victim_output_mint: Pubkey,
    /// Victim's input amount
    pub victim_amount_in: u64,
    /// Victim's min output (slippage tolerance)
    pub victim_min_out: u64,
    /// Calculated victim slippage in bps
    pub victim_slippage_bps: u64,
    /// Our arb route: which pools to use for the backrun
    pub arb_route: ArbRoute,
    /// Expected profit in lamports
    pub expected_profit_lamports: u64,
    /// Flash loan mint to borrow
    pub borrow_mint: Pubkey,
    /// Amount to borrow for the arb
    pub borrow_amount: u64,
}

/// The arbitrage route for a backrun
#[derive(Debug, Clone)]
pub struct ArbRoute {
    /// Leg 1: Buy the token that became cheaper after victim's swap
    pub leg1: ArbLeg,
    /// Leg 2: Sell back for anchor token on another pool
    pub leg2: ArbLeg,
    /// Optimal borrow amount calculated via simulation
    pub optimal_borrow: u64,
    /// Expected profit from this route
    pub expected_profit: u64,
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

/// MEV Evaluator - processes swaps and finds profitable opportunities
pub struct MevEvaluator {
    /// Predictive state manager - THE SINGLE SOURCE OF TRUTH for pool state
    /// Contains confirmed state (from Geyser) + pending swaps (from ShredStream)
    /// Also contains pool metadata and route index
    predictive_state: Arc<PredictiveStateManager>,
    /// Anchor mints we can flash loan
    anchor_mints: Vec<Pubkey>,
    /// Statistics
    pub stats: EvaluatorStats,
}

/// Information about a known pool (metadata only - reserves come from PredictiveStateManager)
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
        }
    }
}

impl MevEvaluator {
    /// Create a new MevEvaluator
    ///
    /// The PredictiveStateManager is THE source of truth for all pool state.
    /// It contains confirmed reserves (from Geyser) plus pending swaps (from ShredStream).
    ///
    /// `anchor_mints` should be the set of mints for which flash loans are configured.
    /// This is typically derived from `flash_loan_reserves.keys()` at startup.
    pub fn new(predictive_state: Arc<PredictiveStateManager>, anchor_mints: Vec<Pubkey>) -> Self {
        tracing::info!(
            "MevEvaluator initialized with {} anchor mints: {:?}",
            anchor_mints.len(),
            anchor_mints.iter().map(|m| m.to_string()).collect::<Vec<_>>()
        );
        Self {
            predictive_state,
            anchor_mints,
            stats: EvaluatorStats::default(),
        }
    }

    /// Get a PoolSimulator for the given pool using PREDICTED state
    ///
    /// This uses curve-specific math (CLMM, DLMM, or constant product)
    /// and includes pending swaps detected via ShredStream.
    fn get_pool_simulator(&self, pool: &Pubkey) -> Option<PoolSimulator> {
        self.predictive_state.get_predicted_simulator(pool)
    }

    /// Get pool reserves (base, quote) using PREDICTED state
    fn get_pool_reserves(&self, pool: &Pubkey) -> Option<(u64, u64)> {
        self.predictive_state.get_pool_reserves(pool)
    }

    /// Get pool info from PredictiveStateManager
    fn get_pool_info(&self, pool: &Pubkey) -> Option<PoolInfo> {
        let (base_mint, quote_mint) = self.predictive_state.get_pool_mints(pool)?;
        let dex = self.predictive_state.get_pool_dex(pool)?;
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
        self.predictive_state.pool_count()
    }

    /// Get a reference to stats
    pub fn stats(&self) -> &EvaluatorStats {
        &self.stats
    }

    /// Evaluate a transaction's swaps for MEV opportunities
    /// Uses atomic stats for lock-free operation (can be called with &self)
    pub fn evaluate(&self, tx_swaps: &TransactionSwaps) -> Vec<MevOpportunity> {
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

            // Filter 3: Calculate victim's slippage
            // Try to calculate slippage from reserves; fall back to min_out ratio if reserves unavailable
            let (victim_slippage_bps, victim_amount_in, victim_min_out) =
                match self.calculate_victim_slippage(swap, &pool_info) {
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
                                "REJECT: could not calculate victim slippage (missing amounts)"
                            );
                            continue;
                        }
                    }
                };

            if victim_slippage_bps < MIN_VICTIM_SLIPPAGE_BPS {
                self.stats.swaps_low_slippage.fetch_add(1, Ordering::Relaxed);
                tracing::trace!(
                    pool = %swap.pool,
                    slippage_bps = victim_slippage_bps,
                    min_required = MIN_VICTIM_SLIPPAGE_BPS,
                    "REJECT: victim slippage too low"
                );
                continue; // Not enough slippage to extract profit
            }

            // Filter 4: Find arb route (need a second pool for the pair)
            let (victim_input, victim_output) = self.infer_swap_mints(swap, &pool_info);

            let arb_route = match self.find_arb_route(
                &swap.pool,
                &victim_input,
                &victim_output,
                victim_amount_in,
            ) {
                Some(route) => route,
                None => {
                    self.stats
                        .swaps_no_arb_route
                        .fetch_add(1, Ordering::Relaxed);
                    // TRACE level: single-pool pairs can't be arbed, this is expected
                    tracing::trace!(
                        pool = %swap.pool,
                        input = %victim_input,
                        output = %victim_output,
                        amount = victim_amount_in,
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
                    "NEAR-MISS: raw_profit={} adjusted={} min={} pool={} victim_amt={}",
                    arb_route.expected_profit,
                    expected_profit,
                    MIN_PROFIT_LAMPORTS,
                    swap.pool,
                    victim_amount_in
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
                    "Backrun rejected: leg2 after slippage {} < flash repay {}",
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
            let opp = MevOpportunity {
                id: format!("{:x}", rand::random::<u64>()),
                detected_at: tx_swaps.received_at,
                victim_tx: tx_swaps.transaction.clone(),
                victim_swap: swap.clone(),
                slot: tx_swaps.slot,
                pool: swap.pool,
                dex: swap.dex,
                victim_input_mint: victim_input,
                victim_output_mint: victim_output,
                victim_amount_in,
                victim_min_out,
                victim_slippage_bps,
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

    /// Calculate the victim's effective slippage from their swap params
    /// Uses predicted state if available for more accurate pricing
    fn calculate_victim_slippage(
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

    /// Find an arbitrage route for the backrun
    ///
    /// CRITICAL: For backrun-only mode (no front-run), single-pool arb is IMPOSSIBLE.
    /// When victim swaps A->B, they push price making B expensive on this pool.
    /// We can only profit with cross-pool arb:
    ///   1. Buy B cheap on Pool Y (unaffected by victim)
    ///   2. Sell B expensive on Pool X (post-victim)
    fn find_arb_route(
        &self,
        victim_pool: &Pubkey,
        victim_input: &Pubkey,
        victim_output: &Pubkey,
        victim_amount: u64,
    ) -> Option<ArbRoute> {
        // The victim sold input and bought output on victim_pool
        // After their swap, output is expensive on victim_pool
        //
        // Cross-pool arb strategy:
        // 1. Find alternate pool trading the same pair
        // 2. Buy output (expensive on victim_pool) cheap on alt_pool
        // 3. Sell output on victim_pool (post-victim) for profit

        // Determine which mint is the anchor (what we borrow)
        let borrow_mint = if self.anchor_mints.contains(victim_input) {
            *victim_input
        } else if self.anchor_mints.contains(victim_output) {
            *victim_output
        } else {
            // Neither is an anchor - need a 3-hop route (not implemented yet)
            tracing::trace!(
                victim_pool = %victim_pool,
                input = %victim_input,
                output = %victim_output,
                "ARB ROUTE: neither mint is an anchor (no flash loan available)"
            );
            return None;
        };

        // Find alternate pool from PredictiveStateManager (single source of truth)
        let alt_pool_state = match self.predictive_state.find_alternate_pool(
            victim_pool,
            victim_input,
            victim_output,
        ) {
            Some(state) => state,
            None => {
                tracing::trace!(
                    victim_pool = %victim_pool,
                    input = %victim_input,
                    output = %victim_output,
                    "No alternate pool found for cross-pool arb"
                );
                return None;
            }
        };

        // Get victim pool info
        let victim_pool_info = self.get_pool_info(victim_pool)?;

        // Get pool mints from predictive state
        let (victim_base_mint, _) = self.predictive_state.get_pool_mints(victim_pool)?;
        let alt_base_mint = alt_pool_state.base_mint;

        // Determine swap directions
        let victim_base_to_quote = *victim_input == victim_base_mint;

        // Get PoolSimulators with curve-specific math (CLMM, DLMM, or constant product)
        // This ensures accurate profit estimation for all pool types
        let pool_x = self.get_pool_simulator(victim_pool)?;
        let pool_y = alt_pool_state.predicted_simulator();

        // Determine if we borrow base or quote relative to pool Y
        let borrow_is_base_on_y = borrow_mint == alt_base_mint;

        // Find optimal borrow amount using unified simulation (curve-aware)
        let max_borrow = victim_amount * 2; // Up to 2x victim size
        let (optimal_borrow, expected_profit) = find_optimal_borrow_unified(
            &pool_x,
            &pool_y,
            victim_amount,
            victim_base_to_quote,
            borrow_is_base_on_y,
            max_borrow,
            30, // 0.3% flash loan fee
        );

        if optimal_borrow == 0 || expected_profit == 0 {
            // No profitable arb found
            return None;
        }

        // Simulate the arb to get leg outputs (using unified curve-aware simulation)
        let simulation = simulate_cross_pool_backrun_unified(
            &pool_x,
            &pool_y,
            victim_amount,
            victim_base_to_quote,
            optimal_borrow,
            borrow_is_base_on_y,
            30,
        )?;

        // Build pool swap states for instruction building (from PSM pool state)
        let leg1_pool_state = self.build_pool_swap_state_from_psm(&alt_pool_state);
        let leg2_pool_state = self.build_pool_swap_state(victim_pool)?;

        // Leg 1: swap on pool Y (unaffected by victim)
        // Leg 2: swap on pool X (post-victim, profitable direction)
        let (leg1_input, leg1_output_mint) = if borrow_is_base_on_y {
            (borrow_mint, *victim_output)
        } else {
            (borrow_mint, *victim_input)
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
                pool: *victim_pool,
                dex: victim_pool_info.dex,
                input_mint: leg2_input,
                output_mint: leg2_output_mint,
                estimated_output: simulation.leg2_output,
                pool_state: leg2_pool_state,
            },
            optimal_borrow,
            expected_profit,
        })
    }

    /// Build PoolSwapState directly from a PoolPredictiveState (from PSM)
    fn build_pool_swap_state_from_psm(
        &self,
        state: &crate::predictive_state::PoolPredictiveState,
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
        if let Some(state) = self.predictive_state.get_predicted_state(pool) {
            if state.has_metadata() {
                return Some(self.build_pool_swap_state_from_psm(&state));
            }
        }

        // Fallback: minimal state from PSM mints (will likely fail on execution)
        let (base_mint, quote_mint) = self.predictive_state.get_pool_mints(pool)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{USDC_MINT, WSOL_MINT};

    fn create_test_predictive_state() -> Arc<PredictiveStateManager> {
        Arc::new(PredictiveStateManager::new())
    }

    #[test]
    fn test_evaluator_creation() {
        let predictive_state = create_test_predictive_state();
        // Test with SOL and USDC as anchor mints
        let anchor_mints = vec![WSOL_MINT, USDC_MINT];
        let evaluator = MevEvaluator::new(predictive_state, anchor_mints);
        assert_eq!(evaluator.anchor_mints.len(), 2);
    }
}
