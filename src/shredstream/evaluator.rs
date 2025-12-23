//! MEV Evaluator - Filters parsed swaps into profitable opportunities
//!
//! Takes ParsedSwaps from shredstream and evaluates them for MEV extraction:
//! - Backrun: Arb after the victim's swap moves the pool price
//! - Sandwich: Front-run + back-run for larger opportunities (optional)

use super::parser::{ParsedSwap, TransactionSwaps};
use super::programs::DexType;
use crate::amm_math::PoolStateCache;
use crate::pool_discovery::DynamicPoolRegistry;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::VersionedTransaction;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Minimum swap amount in lamports to consider for MEV (filters dust)
const MIN_SWAP_AMOUNT_LAMPORTS: u64 = 10_000_000; // 0.01 SOL

/// Minimum expected profit in lamports to submit a bundle
const MIN_PROFIT_LAMPORTS: u64 = 50_000; // 0.00005 SOL

/// Maximum slippage the victim has set (determines our potential profit)
/// We only backrun swaps where the victim accepts significant slippage
const MIN_VICTIM_SLIPPAGE_BPS: u64 = 30; // 0.3%

/// Anchor token mints we can flash loan
/// WSOL and USDC are the primary anchor tokens for flash loans
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

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
}

/// A single leg of the arbitrage
#[derive(Debug, Clone)]
pub struct ArbLeg {
    pub pool: Pubkey,
    pub dex: DexType,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    /// Estimated output after this leg
    pub estimated_output: u64,
}

/// MEV Evaluator - processes swaps and finds profitable opportunities
pub struct MevEvaluator {
    /// Pool state cache for price calculations
    pool_states: Arc<PoolStateCache>,
    /// Dynamic pool registry (replaces static ClmmArrayMap)
    pool_registry: Option<Arc<DynamicPoolRegistry>>,
    /// Pools we know about: pool -> (base_mint, quote_mint)
    known_pools: HashMap<Pubkey, PoolInfo>,
    /// Cross-pool routes: (mint_a, mint_b) -> [pools that trade this pair]
    pool_routes: HashMap<(Pubkey, Pubkey), Vec<PoolInfo>>,
    /// Anchor mints we can flash loan
    anchor_mints: Vec<Pubkey>,
    /// Statistics
    pub stats: EvaluatorStats,
}

/// Information about a known pool
#[derive(Debug, Clone)]
pub struct PoolInfo {
    pub address: Pubkey,
    pub dex: DexType,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    /// Last known reserves (updated from swaps/pool updates)
    pub base_reserve: u64,
    pub quote_reserve: u64,
}

/// Statistics for the evaluator
#[derive(Debug, Default)]
pub struct EvaluatorStats {
    pub swaps_evaluated: u64,
    pub swaps_too_small: u64,
    pub swaps_no_pool_info: u64,
    pub swaps_no_arb_route: u64,
    pub opportunities_found: u64,
    pub opportunities_below_threshold: u64,
}

impl MevEvaluator {
    pub fn new(
        pool_states: Arc<PoolStateCache>,
        pool_registry: Option<Arc<DynamicPoolRegistry>>,
    ) -> Self {
        // Initialize anchor mints
        let anchor_mints = vec![
            WSOL_MINT.parse().unwrap(),
            USDC_MINT.parse().unwrap(),
        ];

        Self {
            pool_states,
            pool_registry,
            known_pools: HashMap::new(),
            pool_routes: HashMap::new(),
            anchor_mints,
            stats: EvaluatorStats::default(),
        }
    }

    /// Register a pool we can use for arbitrage
    pub fn register_pool(&mut self, info: PoolInfo) {
        // Add to known pools
        self.known_pools.insert(info.address, info.clone());

        // Add to route index (both directions)
        let key1 = (info.base_mint, info.quote_mint);
        let key2 = (info.quote_mint, info.base_mint);

        self.pool_routes
            .entry(key1)
            .or_insert_with(Vec::new)
            .push(info.clone());
        self.pool_routes
            .entry(key2)
            .or_insert_with(Vec::new)
            .push(info);
    }

    /// Update pool info from observed swap
    pub fn update_pool_from_swap(&mut self, swap: &ParsedSwap) {
        if let Some(pool_info) = self.known_pools.get_mut(&swap.pool) {
            // Could update reserves here if we track them
            // For now, we rely on PoolStateCache
            let _ = pool_info;
        }
    }

    /// Sync pools from the dynamic pool registry
    /// Call this periodically to pick up newly discovered pools
    pub fn sync_from_registry(&mut self) {
        let Some(registry) = &self.pool_registry else {
            return;
        };

        let pools = registry.all_pools();
        let mut new_pools = 0;
        
        for pool in pools {
            if self.known_pools.contains_key(&pool.address) {
                continue;
            }
            
            // Get reserves from pool state cache if available
            let (base_reserve, quote_reserve) = if let Some(state) = self.pool_states.get(&pool.address.to_string()) {
                (state.base_reserve, state.quote_reserve)
            } else {
                (0, 0) // Will be updated when we see activity
            };

            // Map token_mint_a/b to base_mint/quote_mint
            // For CLMMs, token A is typically the base and B is quote
            let info = PoolInfo {
                address: pool.address,
                dex: pool.dex,
                base_mint: pool.token_mint_a,
                quote_mint: pool.token_mint_b,
                base_reserve,
                quote_reserve,
            };
            
            self.register_pool(info);
            new_pools += 1;
        }

        if new_pools > 0 {
            tracing::debug!("Synced {} new pools from registry (total: {} pools, {} routes)", 
                new_pools, self.known_pools.len(), self.pool_routes.len());
        }
    }

    /// Get stats summary
    pub fn stats_summary(&self) -> String {
        format!(
            "evaluated={} too_small={} no_pool={} no_route={} found={} below_thresh={}",
            self.stats.swaps_evaluated,
            self.stats.swaps_too_small,
            self.stats.swaps_no_pool_info,
            self.stats.swaps_no_arb_route,
            self.stats.opportunities_found,
            self.stats.opportunities_below_threshold
        )
    }

    /// Get number of known pools
    pub fn known_pool_count(&self) -> usize {
        self.known_pools.len()
    }

    /// Get number of arb routes
    pub fn route_count(&self) -> usize {
        self.pool_routes.len()
    }

    /// Get a reference to stats
    pub fn stats(&self) -> &EvaluatorStats {
        &self.stats
    }

    /// Evaluate a transaction's swaps for MEV opportunities
    pub fn evaluate(&mut self, tx_swaps: &TransactionSwaps) -> Vec<MevOpportunity> {
        let mut opportunities = Vec::new();

        for swap in &tx_swaps.swaps {
            self.stats.swaps_evaluated += 1;

            // Filter 1: Minimum amount
            if let Some(amount) = swap.amount_in {
                if amount < MIN_SWAP_AMOUNT_LAMPORTS {
                    self.stats.swaps_too_small += 1;
                    continue;
                }
            } else {
                continue; // No amount = can't evaluate
            }

            // Filter 2: Need pool info for arb route
            let pool_info = match self.known_pools.get(&swap.pool) {
                Some(info) => info.clone(),
                None => {
                    self.stats.swaps_no_pool_info += 1;
                    // Try to infer from PoolStateCache
                    if let Some(state) = self.pool_states.get(&swap.pool.to_string()) {
                        let info = PoolInfo {
                            address: swap.pool,
                            dex: swap.dex,
                            base_mint: state.base_mint.parse().unwrap_or_default(),
                            quote_mint: state.quote_mint.parse().unwrap_or_default(),
                            base_reserve: state.base_reserve,
                            quote_reserve: state.quote_reserve,
                        };
                        self.register_pool(info.clone());
                        info
                    } else {
                        continue;
                    }
                }
            };

            // Filter 3: Calculate victim's slippage
            let (victim_slippage_bps, victim_amount_in, victim_min_out) =
                match self.calculate_victim_slippage(swap, &pool_info) {
                    Some(s) => s,
                    None => continue,
                };

            if victim_slippage_bps < MIN_VICTIM_SLIPPAGE_BPS {
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
                    self.stats.swaps_no_arb_route += 1;
                    continue;
                }
            };

            // Filter 5: Calculate expected profit
            let expected_profit = self.estimate_profit(&arb_route, victim_amount_in);

            if expected_profit < MIN_PROFIT_LAMPORTS {
                self.stats.opportunities_below_threshold += 1;
                continue;
            }

            // Determine flash loan parameters
            let (borrow_mint, borrow_amount) = self.calculate_borrow_params(
                &victim_input,
                &victim_output,
                victim_amount_in,
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
                expected_profit_lamports: expected_profit,
                borrow_mint,
                borrow_amount,
            };

            self.stats.opportunities_found += 1;
            opportunities.push(opp);
        }

        opportunities
    }

    /// Calculate the victim's effective slippage from their swap params
    fn calculate_victim_slippage(
        &self,
        swap: &ParsedSwap,
        pool_info: &PoolInfo,
    ) -> Option<(u64, u64, u64)> {
        let amount_in = swap.amount_in?;
        let min_out = swap.min_amount_out?;

        // Get current pool price to estimate "fair" output
        let pool_addr = swap.pool.to_string();
        let state = self.pool_states.get(&pool_addr)?;

        // Calculate expected output at current price
        let (input_reserve, output_reserve) = if swap.input_mint == Some(pool_info.base_mint) {
            (state.base_reserve, state.quote_reserve)
        } else {
            (state.quote_reserve, state.base_reserve)
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
    /// After victim swaps A->B on Pool1, we want to:
    /// 1. Buy B cheap on Pool1 (price moved in our favor)
    /// 2. Sell B for A on Pool2 (or back to anchor token)
    fn find_arb_route(
        &self,
        victim_pool: &Pubkey,
        victim_input: &Pubkey,
        victim_output: &Pubkey,
        amount: u64,
    ) -> Option<ArbRoute> {
        // The victim sold input and bought output
        // After their swap, output is more expensive on this pool
        // We want to: buy output on this pool (now cheaper after we're placed after victim)
        //             sell output on another pool

        // Look for another pool with the same pair
        let route_key = (*victim_output, *victim_input);
        let alt_pools = self.pool_routes.get(&route_key)?;

        // Find a pool that isn't the victim's pool
        let alt_pool = alt_pools
            .iter()
            .find(|p| p.address != *victim_pool)?;

        // Determine which mint is the anchor (what we borrow)
        let borrow_mint = if self.anchor_mints.contains(victim_input) {
            *victim_input
        } else if self.anchor_mints.contains(victim_output) {
            *victim_output
        } else {
            // Neither is an anchor - need a 3-hop route (not implemented yet)
            return None;
        };

        // Calculate estimated outputs
        // Leg 1: Borrow anchor -> swap for output token on victim's pool
        // Leg 2: Swap output token -> anchor on alt pool
        let leg1_output = self.estimate_swap_output(
            victim_pool,
            &borrow_mint,
            victim_output,
            amount,
        )?;

        let leg2_output = self.estimate_swap_output(
            &alt_pool.address,
            victim_output,
            &borrow_mint,
            leg1_output,
        )?;

        Some(ArbRoute {
            leg1: ArbLeg {
                pool: *victim_pool,
                dex: self.known_pools.get(victim_pool)?.dex,
                input_mint: borrow_mint,
                output_mint: *victim_output,
                estimated_output: leg1_output,
            },
            leg2: ArbLeg {
                pool: alt_pool.address,
                dex: alt_pool.dex,
                input_mint: *victim_output,
                output_mint: borrow_mint,
                estimated_output: leg2_output,
            },
        })
    }

    /// Estimate swap output using pool state
    fn estimate_swap_output(
        &self,
        pool: &Pubkey,
        input_mint: &Pubkey,
        _output_mint: &Pubkey,
        amount_in: u64,
    ) -> Option<u64> {
        let pool_addr = pool.to_string();
        let input_mint_str = input_mint.to_string();

        // Use CLMM-aware calculation if available
        // Note: pool_registry provides tick_spacing, but the actual calculation
        // now uses unified logic in PoolStateCache
        if let Some(quote) = self.pool_states.calculate_clmm_swap_output(
            &pool_addr,
            amount_in,
            &input_mint_str,
            None, // No longer using static ClmmArrayMap
            None,
            None,
        ) {
            return Some(quote.output_amount);
        }

        // Fallback to simple constant product estimate
        let state = self.pool_states.get(&pool_addr)?;
        let base_mint: Pubkey = state.base_mint.parse().ok()?;

        let (input_reserve, output_reserve) = if *input_mint == base_mint {
            (state.base_reserve, state.quote_reserve)
        } else {
            (state.quote_reserve, state.base_reserve)
        };

        if input_reserve == 0 || output_reserve == 0 {
            return None;
        }

        // Apply 0.3% fee estimate
        let amount_with_fee = amount_in as u128 * 997;
        let numerator = amount_with_fee * output_reserve as u128;
        let denominator = input_reserve as u128 * 1000 + amount_with_fee;

        Some((numerator / denominator) as u64)
    }

    /// Estimate profit from the arb route
    fn estimate_profit(&self, route: &ArbRoute, borrow_amount: u64) -> u64 {
        let final_output = route.leg2.estimated_output;

        // Profit = final_output - borrow_amount - flash_loan_fee
        // Flash loan fee is ~0.3% = 30 bps
        let flash_fee = borrow_amount * 30 / 10000;
        let total_cost = borrow_amount + flash_fee;

        if final_output > total_cost {
            final_output - total_cost
        } else {
            0
        }
    }

    /// Calculate flash loan borrow parameters
    fn calculate_borrow_params(
        &self,
        victim_input: &Pubkey,
        victim_output: &Pubkey,
        victim_amount: u64,
    ) -> (Pubkey, u64) {
        // Borrow the anchor token
        let borrow_mint = if self.anchor_mints.contains(victim_input) {
            *victim_input
        } else if self.anchor_mints.contains(victim_output) {
            *victim_output
        } else {
            // Default to WSOL
            self.anchor_mints[0]
        };

        // Borrow ~50% of victim's trade size (conservative)
        let borrow_amount = victim_amount / 2;

        (borrow_mint, borrow_amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluator_creation() {
        let pool_states = Arc::new(PoolStateCache::new());
        let evaluator = MevEvaluator::new(pool_states, None);
        assert_eq!(evaluator.anchor_mints.len(), 2);
    }
}
