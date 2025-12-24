//! MEV Evaluator - Filters parsed swaps into profitable opportunities
//!
//! Takes ParsedSwaps from shredstream and evaluates them for MEV extraction:
//! - Backrun: Arb after the victim's swap moves the pool price
//! - Sandwich: Front-run + back-run for larger opportunities (optional)

use super::parser::{ParsedSwap, TransactionSwaps};
use super::programs::DexType;
use crate::amm_math::{
    PoolStateCache, VirtualPoolState, 
    simulate_cross_pool_backrun, find_optimal_cross_pool_borrow,
    simulate_sandwich, find_optimal_sandwich_amount,
};
use crate::pool_discovery::DynamicPoolRegistry;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::VersionedTransaction;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Minimum swap amount in lamports to consider for MEV (filters dust)
/// Lowered to catch more opportunities - smaller swaps can still be profitable
const MIN_SWAP_AMOUNT_LAMPORTS: u64 = 1_000_000; // 0.001 SOL (was 0.01)

/// Minimum expected profit in lamports to submit a bundle
/// After gas costs (~5000 lamports) we need at least this profit
const MIN_PROFIT_LAMPORTS: u64 = 10_000; // 0.00001 SOL (was 50000)

/// Maximum slippage the victim has set (determines our potential profit)
/// Lower threshold catches more opportunities but may have lower success rate
const MIN_VICTIM_SLIPPAGE_BPS: u64 = 10; // 0.1% (was 0.3%)

/// Anti-sandwich prefix that Jito uses to protect transactions
/// Transactions with accounts starting with this prefix (case-insensitive)
/// cannot be sandwiched via Jito block engine.
/// See: https://docs.jito.wtf/lowlatencytxnsend/#sandwich-mitigation
const JITO_DONT_FRONT_PREFIX: &str = "jitodontfront";

/// Anchor token mints we can flash loan
/// WSOL and USDC are the primary anchor tokens for flash loans
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
// Additional anchor tokens for wider coverage
pub const USDT_MINT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
pub const MSOL_MINT: &str = "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So";
pub const JITOSOL_MINT: &str = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";

/// Check if a transaction uses Jito's anti-sandwich protection.
/// Returns true if any account in the transaction starts with "jitodontfront" (case-insensitive).
fn has_jitodontfront_protection(tx: &VersionedTransaction) -> bool {
    let account_keys = tx.message.static_account_keys();
    for key in account_keys {
        let key_str = key.to_string();
        if key_str.to_lowercase().starts_with(JITO_DONT_FRONT_PREFIX) {
            return true;
        }
    }
    false
}

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

/// A sandwich MEV opportunity (frontrun + victim + backrun on same pool)
#[derive(Debug, Clone)]
pub struct SandwichOpportunity {
    /// Unique ID for tracking
    pub id: String,
    /// When this opportunity was detected
    pub detected_at: Instant,
    /// The victim transaction
    pub victim_tx: VersionedTransaction,
    /// The victim's swap details
    pub victim_swap: ParsedSwap,
    /// Slot where victim tx was seen
    pub slot: u64,
    /// The pool being sandwiched
    pub pool: Pubkey,
    /// DEX type for instruction building
    pub dex: DexType,
    /// Token the victim is selling
    pub victim_input_mint: Pubkey,
    /// Token the victim is buying
    pub victim_output_mint: Pubkey,
    /// Victim's input amount
    pub victim_amount_in: u64,
    /// Victim's min output (slippage tolerance)
    pub victim_min_out: u64,
    /// Our frontrun amount (same direction as victim)
    pub frontrun_amount: u64,
    /// Expected output from frontrun
    pub frontrun_output: u64,
    /// Expected output from backrun (our profit source)
    pub backrun_output: u64,
    /// Expected profit in lamports
    pub expected_profit_lamports: u64,
    /// Flash loan mint to borrow
    pub borrow_mint: Pubkey,
    /// Pool state for building swap instructions
    pub pool_state: PoolSwapState,
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
    /// Whether sandwich attacks are enabled
    sandwich_enabled: bool,
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
    /// Sandwich-specific stats
    pub sandwich_opportunities_found: u64,
    pub sandwich_victim_would_fail: u64,
    /// Transactions filtered due to jitodontfront anti-sandwich protection
    pub sandwich_jitodontfront_filtered: u64,
}

impl MevEvaluator {
    pub fn new(
        pool_states: Arc<PoolStateCache>,
        pool_registry: Option<Arc<DynamicPoolRegistry>>,
        sandwich_enabled: bool,
    ) -> Self {
        // Initialize anchor mints - tokens we can flash loan
        let anchor_mints = vec![
            WSOL_MINT.parse().unwrap(),
            USDC_MINT.parse().unwrap(),
            // Add more anchor tokens if flash loan reserves are available
            USDT_MINT.parse().unwrap_or_default(),
            MSOL_MINT.parse().unwrap_or_default(),
            JITOSOL_MINT.parse().unwrap_or_default(),
        ].into_iter().filter(|p| *p != Pubkey::default()).collect();

        Self {
            pool_states,
            pool_registry,
            known_pools: HashMap::new(),
            pool_routes: HashMap::new(),
            anchor_mints,
            sandwich_enabled,
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
            "evaluated={} too_small={} no_pool={} no_route={} found={} sandwich={} below_thresh={} jitodontfront={}",
            self.stats.swaps_evaluated,
            self.stats.swaps_too_small,
            self.stats.swaps_no_pool_info,
            self.stats.swaps_no_arb_route,
            self.stats.opportunities_found,
            self.stats.sandwich_opportunities_found,
            self.stats.opportunities_below_threshold,
            self.stats.sandwich_jitodontfront_filtered
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
                tracing::trace!(
                    pool = %swap.pool,
                    slippage_bps = victim_slippage_bps,
                    min_required = MIN_VICTIM_SLIPPAGE_BPS,
                    "Skipping: victim slippage too low"
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
                    self.stats.swaps_no_arb_route += 1;
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
            let expected_profit = arb_route.expected_profit
                .saturating_mul(10000 - ARB_SLIPPAGE_BUFFER_BPS) / 10000;

            if expected_profit < MIN_PROFIT_LAMPORTS {
                self.stats.opportunities_below_threshold += 1;
                continue;
            }
            
            // CRITICAL: Verify leg2 output after slippage covers flash loan repay
            // Flash loan repay = borrow_amount + 0.3% fee
            let flash_loan_repay = borrow_amount
                .saturating_add(borrow_amount / 333); // ~0.3% fee
            let leg2_after_slippage = arb_route.leg2.estimated_output
                .saturating_mul(10000 - 300) / 10000; // 3% slippage on leg2
            
            if leg2_after_slippage < flash_loan_repay {
                tracing::debug!(
                    "Backrun rejected: leg2 after slippage {} < flash repay {}",
                    leg2_after_slippage,
                    flash_loan_repay
                );
                self.stats.opportunities_below_threshold += 1;
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

            self.stats.opportunities_found += 1;
            opportunities.push(opp);
        }

        opportunities
    }

    /// Evaluate a transaction's swaps for sandwich opportunities
    /// Sandwich = frontrun + victim + backrun on the SAME pool
    pub fn evaluate_sandwich(&mut self, tx_swaps: &TransactionSwaps) -> Vec<SandwichOpportunity> {
        if !self.sandwich_enabled {
            return Vec::new();
        }

        // Filter: Skip transactions with jitodontfront anti-sandwich protection
        // These use Jito's sandwich mitigation and cannot be sandwiched via block engine
        if has_jitodontfront_protection(&tx_swaps.transaction) {
            self.stats.sandwich_jitodontfront_filtered += 1;
            return Vec::new();
        }

        let mut opportunities = Vec::new();

        for swap in &tx_swaps.swaps {
            // Filter 1: Minimum amount
            let victim_amount_in = match swap.amount_in {
                Some(amount) if amount >= MIN_SWAP_AMOUNT_LAMPORTS => amount,
                _ => continue,
            };

            // Filter 2: Need pool info
            let pool_info = match self.known_pools.get(&swap.pool) {
                Some(info) => info.clone(),
                None => {
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

            // Filter 3: Get victim's min_out
            let victim_min_out = match swap.min_amount_out {
                Some(min) => min,
                None => continue,
            };

            // Get pool state and create virtual pool
            let pool_addr = swap.pool.to_string();
            let state = match self.pool_states.get(&pool_addr) {
                Some(s) => s,
                None => continue,
            };

            // Determine swap direction
            let (victim_input, victim_output) = self.infer_swap_mints(swap, &pool_info);
            let victim_base_to_quote = victim_input == pool_info.base_mint;

            // Determine borrow mint (must be an anchor token)
            let borrow_mint = if self.anchor_mints.contains(&victim_input) {
                victim_input
            } else if self.anchor_mints.contains(&victim_output) {
                // For sandwich, we typically want to frontrun in same direction as victim
                // So we borrow the input token
                victim_input
            } else {
                continue; // Neither token is an anchor, can't flash loan
            };

            // Create virtual pool state
            let pool = VirtualPoolState::new(
                state.base_reserve,
                state.quote_reserve,
                state.fee_bps,
            );

            // Find optimal sandwich amount that doesn't break victim's min_out
            // IMPORTANT: Cap max_borrow to a reasonable amount based on available capital
            // Sandwiches require upfront capital (no flash loans across tx boundaries!)
            // For now, cap at 100 SOL worth to avoid unrealistic opportunities
            const MAX_SANDWICH_CAPITAL_LAMPORTS: u64 = 100_000_000_000; // 100 SOL
            let max_borrow = (victim_amount_in * 2) // Up to 2x victim size
                .min(MAX_SANDWICH_CAPITAL_LAMPORTS);
            
            let (optimal_amount, expected_profit) = find_optimal_sandwich_amount(
                &pool,
                victim_amount_in,
                victim_base_to_quote,
                victim_min_out,
                max_borrow,
                0, // No flash loan fee for self-funded sandwich
            );

            // Apply slippage buffer to expected profit (conservative estimate)
            // Real-world slippage can be 3-6%, so require 10% buffer
            const PROFIT_SLIPPAGE_BUFFER_BPS: u64 = 1000; // 10% safety margin
            let profit_after_slippage = expected_profit
                .saturating_mul(10000 - PROFIT_SLIPPAGE_BUFFER_BPS) / 10000;

            if optimal_amount == 0 || profit_after_slippage < MIN_PROFIT_LAMPORTS {
                self.stats.opportunities_below_threshold += 1;
                continue;
            }

            // Simulate to get exact outputs
            let result = match simulate_sandwich(
                &pool,
                victim_amount_in,
                victim_base_to_quote,
                optimal_amount,
                0, // No flash loan fee
            ) {
                Some(r) => r,
                None => continue,
            };

            // Verify victim still gets their min_out after our frontrun
            // This is critical - we simulate victim's swap after our frontrun
            let (frontrun_output, pool_after_frontrun) = pool.simulate_swap(optimal_amount, victim_base_to_quote);
            let (victim_gets, _) = pool_after_frontrun.simulate_swap(victim_amount_in, victim_base_to_quote);
            
            if victim_gets < victim_min_out {
                self.stats.sandwich_victim_would_fail += 1;
                continue;
            }
            
            // CRITICAL: Verify backrun output covers frontrun input after slippage
            // Apply 5% slippage tolerance to backrun output
            const BACKRUN_SLIPPAGE_BPS: u64 = 500; // 5%
            let min_backrun_after_slippage = result.leg2_output
                .saturating_mul(10000 - BACKRUN_SLIPPAGE_BPS) / 10000;
            
            if min_backrun_after_slippage < optimal_amount {
                tracing::debug!(
                    "Sandwich rejected: backrun after slippage {} < frontrun {}",
                    min_backrun_after_slippage,
                    optimal_amount
                );
                self.stats.opportunities_below_threshold += 1;
                continue;
            }

            // Build pool swap state for instruction building
            let pool_swap_state = match self.build_pool_swap_state(&swap.pool) {
                Some(s) => s,
                None => continue,
            };

            // Use the slippage-adjusted profit for reporting
            tracing::info!(
                "SANDWICH FOUND: profit={} lamports ({:.6} SOL), frontrun={}, victim_gets={}",
                profit_after_slippage,
                profit_after_slippage as f64 / 1e9,
                optimal_amount,
                victim_gets
            );

            let opp = SandwichOpportunity {
                id: format!("sw_{:x}", rand::random::<u64>()),
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
                frontrun_amount: optimal_amount,
                frontrun_output,
                backrun_output: result.leg2_output,
                expected_profit_lamports: profit_after_slippage, // Use conservative estimate
                borrow_mint,
                pool_state: pool_swap_state,
            };

            self.stats.sandwich_opportunities_found += 1;
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
            return None;
        };

        // MUST find an alternate pool - single-pool backrun is mathematically impossible
        // Try both key orderings since we store routes bidirectionally but might have gaps
        let route_key1 = (*victim_input, *victim_output);
        let route_key2 = (*victim_output, *victim_input);
        
        // First try our local route cache
        let alt_pool_from_cache = self.pool_routes.get(&route_key1)
            .and_then(|pools| pools.iter().find(|p| p.address != *victim_pool).cloned())
            .or_else(|| {
                // Try reverse key
                self.pool_routes.get(&route_key2)
                    .and_then(|pools| pools.iter().find(|p| p.address != *victim_pool).cloned())
            });
        
        // If not found in cache, also try the dynamic registry
        let alt_pool = alt_pool_from_cache.or_else(|| {
            self.pool_registry.as_ref().and_then(|registry| {
                let pool_addrs = registry.get_pools_for_pair(victim_input, victim_output);
                for addr in pool_addrs {
                    if addr != *victim_pool {
                        // Get pool info from registry
                        if let Some(discovered) = registry.get(&addr) {
                            // Check if we have state for this pool
                            if self.pool_states.get(&addr.to_string()).is_some() {
                                let info = PoolInfo {
                                    address: discovered.address,
                                    dex: discovered.dex,
                                    base_mint: discovered.token_mint_a,
                                    quote_mint: discovered.token_mint_b,
                                    base_reserve: 0, // Will use from pool_states
                                    quote_reserve: 0,
                                };
                                return Some(info);
                            }
                        }
                    }
                }
                None
            })
        });
        
        let alt_pool = match alt_pool {
            Some(p) => p,
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
        
        // Get pool states
        let victim_pool_addr = victim_pool.to_string();
        let alt_pool_addr = alt_pool.address.to_string();
        
        let victim_state = self.pool_states.get(&victim_pool_addr)?;
        let alt_state = self.pool_states.get(&alt_pool_addr)?;
        
        // Get pool info for direction
        let victim_pool_info = self.known_pools.get(victim_pool)?;
        
        // Create virtual pool states for simulation
        let victim_base_mint: Pubkey = victim_state.base_mint.parse().ok()?;
        let alt_base_mint: Pubkey = alt_state.base_mint.parse().ok()?;
        
        // Determine swap directions
        let victim_base_to_quote = *victim_input == victim_base_mint;
        
        // Create virtual states
        let pool_x = VirtualPoolState::new(
            victim_state.base_reserve,
            victim_state.quote_reserve,
            victim_state.fee_bps,
        );
        let pool_y = VirtualPoolState::new(
            alt_state.base_reserve,
            alt_state.quote_reserve,
            alt_state.fee_bps,
        );
        
        // Determine if we borrow base or quote relative to pool Y
        let borrow_is_base_on_y = borrow_mint == alt_base_mint;
        
        // Find optimal borrow amount (use binary search from amm_math)
        let max_borrow = victim_amount * 2; // Up to 2x victim size
        let (optimal_borrow, expected_profit) = find_optimal_cross_pool_borrow(
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
        
        // Simulate the arb to get leg outputs
        let simulation = simulate_cross_pool_backrun(
            &pool_x,
            &pool_y,
            victim_amount,
            victim_base_to_quote,
            optimal_borrow,
            borrow_is_base_on_y,
            30,
        )?;
        
        // Build pool swap states for instruction building
        let leg1_pool_state = self.build_pool_swap_state(&alt_pool.address)?;
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
                pool: alt_pool.address,
                dex: alt_pool.dex,
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
    /// The route already contains properly simulated outputs from VirtualPoolState,
    /// so we just need to calculate the net profit after flash loan fees
    fn estimate_profit(&self, route: &ArbRoute, borrow_amount: u64) -> u64 {
        let final_output = route.leg2.estimated_output;

        // Sanity check: if final_output is impossibly large, the calculation is broken
        // Max realistic output is ~10,000 SOL (10e12 lamports)
        const MAX_REALISTIC_OUTPUT: u64 = 10_000_000_000_000; // 10,000 SOL
        if final_output > MAX_REALISTIC_OUTPUT || borrow_amount > MAX_REALISTIC_OUTPUT {
            tracing::warn!(
                "PROFIT CALC: unrealistic values - final_output={} borrow={}",
                final_output, borrow_amount
            );
            return 0;
        }

        // Log for debugging
        tracing::debug!(
            "PROFIT CALC: leg1_out={} leg2_out={} borrow={}",
            route.leg1.estimated_output, final_output, borrow_amount
        );

        // Profit = final_output - borrow_amount - flash_loan_fee
        // Flash loan fee is ~0.3% = 30 bps
        let flash_fee = borrow_amount * 30 / 10000;
        let total_cost = borrow_amount.saturating_add(flash_fee);

        if final_output > total_cost {
            let profit = final_output - total_cost;
            // Cap profit at 100 SOL max (anything higher is likely a bug)
            const MAX_PROFIT: u64 = 100_000_000_000; // 100 SOL
            let capped = profit.min(MAX_PROFIT);
            
            tracing::info!(
                "PROFIT: {} lamports ({:.6} SOL) from borrow={} -> leg1={} -> leg2={}",
                capped, 
                capped as f64 / 1e9,
                borrow_amount,
                route.leg1.estimated_output,
                final_output
            );
            
            capped
        } else {
            0
        }
    }

    /// Build PoolSwapState from the pool registry
    fn build_pool_swap_state(&self, pool: &Pubkey) -> Option<PoolSwapState> {
        // Try to get pool info from the dynamic registry
        if let Some(ref registry) = self.pool_registry {
            if let Some(discovered) = registry.get(pool) {
                return Some(PoolSwapState {
                    token_mint_a: discovered.token_mint_a,
                    token_mint_b: discovered.token_mint_b,
                    token_vault_a: discovered.token_vault_a,
                    token_vault_b: discovered.token_vault_b,
                    current_tick_or_bin: discovered.current_tick_or_bin,
                    spacing: discovered.spacing,
                    // Use pre-computed values from pool discovery
                    amm_authority: discovered.amm_authority,
                    amm_config: discovered.amm_config,
                    observation_state: discovered.observation_state,
                    tick_arrays: discovered.tick_arrays.clone(),
                    bin_array_bitmap_extension: discovered.bin_array_bitmap_extension,
                    bin_arrays: discovered.bin_arrays.clone(),
                    pool_authority: discovered.pool_authority,
                });
            }
        }

        // Fallback: create minimal state from known_pools
        let pool_info = self.known_pools.get(pool)?;
        Some(PoolSwapState {
            token_mint_a: pool_info.base_mint,
            token_mint_b: pool_info.quote_mint,
            token_vault_a: Pubkey::default(), // Will cause error if used without real data
            token_vault_b: Pubkey::default(),
            current_tick_or_bin: 0,
            spacing: 0,
            amm_authority: None, // Will cause error if used
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

    #[test]
    fn test_evaluator_creation() {
        let pool_states = Arc::new(PoolStateCache::new());
        let evaluator = MevEvaluator::new(pool_states, None, false);
        assert_eq!(evaluator.anchor_mints.len(), 2);
    }

    #[test]
    fn test_evaluator_with_sandwich() {
        let pool_states = Arc::new(PoolStateCache::new());
        let evaluator = MevEvaluator::new(pool_states, None, true);
        assert!(evaluator.sandwich_enabled);
    }
}
