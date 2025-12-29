//! AMM math for calculating expected swap outputs with slippage.
//!
//! Supports multiple curve types:
//! - Constant Product (x * y = k): Raydium AMM, Orca Legacy
//! - Concentrated Liquidity (sqrt_price/liquidity based): Raydium CLMM, Orca Whirlpool, PancakeSwap CLMM, Meteora DAMM v2
//! - Discrete Liquidity (bin-based): Meteora DLMM

use crate::clmm_math::ClmmPoolState;
use crate::dlmm_math::DlmmPoolState;

/// Curve types with their swap math
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveType {
    /// x * y = k (Raydium AMM, Orca Legacy)
    ConstantProduct,
    /// Concentrated liquidity with sqrt_price/liquidity (Raydium CLMM, Orca Whirlpool, PancakeSwap CLMM, Meteora DAMM v2)
    /// Reference: MeteoraAg/damm-v2 - uses √P' = √P * L / (L + Δa * √P)
    ConcentratedLiquidity,
    /// Discrete liquidity bins (Meteora DLMM)
    Dlmm,
}

impl Default for CurveType {
    fn default() -> Self {
        CurveType::ConstantProduct
    }
}

/// Result of a swap quote calculation
#[derive(Clone, Debug)]
pub struct SwapQuote {
    pub input_amount: u64,
    pub output_amount: u64,
    pub fee_amount: u64,
    pub price_impact_bps: u64,
    pub input_reserve: u64,
    pub output_reserve: u64,
}

impl SwapQuote {
    /// Calculate effective price (output per input)
    pub fn effective_price(&self) -> f64 {
        if self.input_amount == 0 {
            return 0.0;
        }
        self.output_amount as f64 / self.input_amount as f64
    }

    /// Calculate spot price before trade (for comparison)
    pub fn spot_price(&self) -> f64 {
        if self.input_reserve == 0 {
            return 0.0;
        }
        self.output_reserve as f64 / self.input_reserve as f64
    }
}

/// Spot price from pool reserves (for arb detection)
#[derive(Clone, Debug)]
pub struct SpotPrice {
    pub pool_address: String,
    pub price: f64, // Normalized price (output per input, accounting for decimals)
    pub input_reserve: u64,
    pub output_reserve: u64,
    pub curve: CurveType,
    pub fee_bps: u16,
}

impl SpotPrice {
    /// Estimate how much output we'd get for a given input amount
    /// This is a quick approximation; use calculate_swap_output for accuracy
    pub fn estimate_output(&self, input_amount: u64) -> u64 {
        // Apply fee
        let input_after_fee = input_amount as u128 * (10_000 - self.fee_bps as u128) / 10_000;

        // Constant product output
        let output = (self.output_reserve as u128 * input_after_fee)
            / (self.input_reserve as u128 + input_after_fee);

        output as u64
    }

    /// Calculate liquidity depth - how much input moves price by 1%
    pub fn liquidity_depth(&self) -> u64 {
        // For constant product, 1% price impact requires ~1% of reserves
        // input_for_1pct ≈ input_reserve * 0.01 / (1 - 0.01) ≈ input_reserve * 0.0101
        (self.input_reserve as f64 * 0.0101) as u64
    }
}

/// Constant product AMM: x * y = k
/// Returns (output_amount, price_impact_bps)
#[inline]
fn calculate_constant_product(
    input_amount: u64,
    input_reserve: u64,
    output_reserve: u64,
) -> (u64, u64) {
    if input_reserve == 0 || output_reserve == 0 || input_amount == 0 {
        return (0, 0);
    }

    // Spot price before trade
    let spot_price = output_reserve as f64 / input_reserve as f64;

    // x * y = k
    // (x + dx) * (y - dy) = k
    // dy = y - k / (x + dx)
    // dy = y - (x * y) / (x + dx)
    // dy = y * dx / (x + dx)

    let input_128 = input_amount as u128;
    let in_reserve_128 = input_reserve as u128;
    let out_reserve_128 = output_reserve as u128;

    // output = (output_reserve * input_amount) / (input_reserve + input_amount)
    let numerator = out_reserve_128.saturating_mul(input_128);
    let denominator = in_reserve_128.saturating_add(input_128);
    let output_amount = if denominator == 0 { 0 } else { (numerator / denominator) as u64 };

    // Effective price after trade
    let effective_price = if input_amount > 0 {
        output_amount as f64 / input_amount as f64
    } else {
        spot_price
    };

    // Price impact = (spot_price - effective_price) / spot_price * 10000
    let price_impact_bps = if spot_price > 0.0 {
        ((spot_price - effective_price) / spot_price * 10_000.0) as u64
    } else {
        0
    };

    (output_amount, price_impact_bps)
}

/// Calculate the maximum input amount that keeps slippage under a target
pub fn max_input_for_slippage(
    input_reserve: u64,
    output_reserve: u64,
    max_slippage_bps: u64,
) -> u64 {
    if input_reserve == 0 || output_reserve == 0 {
        return 0;
    }

    // For constant product: slippage ≈ input_amount / (2 * input_reserve)
    // So: input_amount ≈ 2 * input_reserve * max_slippage_bps / 10000
    //
    // More precisely, for price impact P:
    // P = input_amount / (input_reserve + input_amount)
    // Solving: input_amount = P * input_reserve / (1 - P)

    let max_slippage_fraction = max_slippage_bps as f64 / 10_000.0;
    if max_slippage_fraction >= 1.0 {
        return input_reserve; // Can trade entire reserve
    }

    let max_input =
        (max_slippage_fraction * input_reserve as f64 / (1.0 - max_slippage_fraction)) as u64;

    max_input
}

/// Map protocol name to curve type
pub fn curve_type_from_protocol(protocol: &str) -> CurveType {
    let protocol_lower = protocol.to_lowercase();

    if protocol_lower.contains("dlmm") {
        CurveType::Dlmm
    } else if protocol_lower.contains("clmm")
        || protocol_lower.contains("whirlpool")
        || protocol_lower.contains("amm_v3")
    // Raydium CLMM is called amm_v3
    {
        CurveType::ConcentratedLiquidity
    } else {
        // Default: Raydium AMM, Orca Legacy, Meteora DAMM, etc.
        CurveType::ConstantProduct
    }
}

// =============================================================================
// VIRTUAL POOL STATE FOR SWAP SIMULATION
// =============================================================================

/// Virtual pool state for simulating swaps without modifying actual cache.
/// Used to calculate arb profit by simulating trigger's swap then our arb legs.
#[derive(Clone, Debug)]
pub struct VirtualPoolState {
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub fee_bps: u16,
}

impl VirtualPoolState {
    /// Create with explicit values
    pub fn new(base_reserve: u64, quote_reserve: u64, fee_bps: u16) -> Self {
        Self {
            base_reserve,
            quote_reserve,
            fee_bps,
        }
    }

    /// Simulate a swap and return (output_amount, new_state)
    ///
    /// # Arguments
    /// * `amount_in` - Input amount in base units
    /// * `base_to_quote` - true = selling base for quote, false = selling quote for base
    ///
    /// # Returns
    /// * `(output_amount, new_virtual_state)` - The output and the pool state after the swap
    pub fn simulate_swap(&self, amount_in: u64, base_to_quote: bool) -> (u64, VirtualPoolState) {
        if amount_in == 0 {
            return (0, self.clone());
        }

        let (input_reserve, output_reserve) = if base_to_quote {
            (self.base_reserve, self.quote_reserve)
        } else {
            (self.quote_reserve, self.base_reserve)
        };

        // Check for zero reserves
        if input_reserve == 0 || output_reserve == 0 {
            return (0, self.clone());
        }

        // Apply fee to input
        let fee_amount = (amount_in as u128 * self.fee_bps as u128 / 10_000) as u64;
        let input_after_fee = amount_in.saturating_sub(fee_amount);

        // Constant product: output = (output_reserve * input) / (input_reserve + input)
        let numerator = output_reserve as u128 * input_after_fee as u128;
        let denominator = input_reserve as u128 + input_after_fee as u128;
        let output = (numerator / denominator) as u64;

        // Ensure we don't drain the pool
        let output = output.min(output_reserve.saturating_sub(1));

        // Calculate new reserves
        // NOTE: Per Raydium AMM reference (Calculator::swap_token_amount_base_in),
        // the full input amount is added to reserves, and fees are tracked separately
        // in swap_acc_coin_fee / swap_acc_pc_fee. However, for simulation purposes
        // where we don't track fee accumulation, we add input_after_fee to avoid
        // overstating liquidity depth.
        let new_state = if base_to_quote {
            VirtualPoolState {
                base_reserve: self.base_reserve.saturating_add(input_after_fee),
                quote_reserve: self.quote_reserve.saturating_sub(output),
                fee_bps: self.fee_bps,
            }
        } else {
            VirtualPoolState {
                base_reserve: self.base_reserve.saturating_sub(output),
                quote_reserve: self.quote_reserve.saturating_add(input_after_fee),
                fee_bps: self.fee_bps,
            }
        };

        (output, new_state)
    }

    /// Get the spot price (quote per base)
    pub fn spot_price(&self) -> f64 {
        if self.base_reserve == 0 {
            return 0.0;
        }
        self.quote_reserve as f64 / self.base_reserve as f64
    }

    /// Calculate expected output without modifying state
    pub fn calculate_output(&self, amount_in: u64, base_to_quote: bool) -> u64 {
        self.simulate_swap(amount_in, base_to_quote).0
    }
}

/// Result of simulating an arbitrage opportunity
#[derive(Clone, Debug)]
pub struct ArbSimulationResult {
    /// Profit in the borrow token (lamports or base units)
    pub profit: u64,
    /// Amount to borrow for the arb
    pub borrow_amount: u64,
    /// Output from leg 1
    pub leg1_output: u64,
    /// Output from leg 2 (what we get back)
    pub leg2_output: u64,
    /// Output from leg 3 (for 3-hop arbs, otherwise 0)
    pub leg3_output: u64,
    /// Flash loan fee
    pub flash_loan_fee: u64,
    /// Is this a cross-pool arb (true) or sandwich (false)?
    pub is_cross_pool: bool,
}









// =============================================================================
// UNIFIED POOL SIMULATOR (dispatches to correct curve math)
// =============================================================================

/// Unified pool simulator that dispatches to the correct curve math.
///
/// This allows cross-pool arbitrage simulation to work correctly across
/// different DEX types (AMM, CLMM, DLMM) rather than forcing constant product.
#[derive(Clone, Debug)]
pub enum PoolSimulator {
    /// Constant product AMM (Raydium AMM)
    ConstantProduct(VirtualPoolState),
    /// Concentrated liquidity (Raydium CLMM, Orca Whirlpool, Meteora DAMM v2)
    Clmm(ClmmPoolState),
    /// Dynamic liquidity market maker (Meteora DLMM)
    Dlmm(DlmmPoolState),
}

impl PoolSimulator {
    /// Simulate a swap and return (output_amount, new_state)
    ///
    /// # Arguments
    /// * `amount_in` - Input amount
    /// * `base_to_quote` - true = swap base for quote, false = swap quote for base
    ///
    /// # Returns
    /// * `(output_amount, new_state)` - The output and updated simulator state
    pub fn simulate_swap(&self, amount_in: u64, base_to_quote: bool) -> (u64, PoolSimulator) {
        match self {
            PoolSimulator::ConstantProduct(state) => {
                let (output, new_state) = state.simulate_swap(amount_in, base_to_quote);
                (output, PoolSimulator::ConstantProduct(new_state))
            }
            PoolSimulator::Clmm(state) => {
                // zero_for_one = base_to_quote (selling token0 for token1)
                let zero_for_one = base_to_quote;
                let (amount_in_used, amount_out, new_state) = state.simulate_swap(
                    amount_in,
                    zero_for_one,
                    true, // is_base_input (exact input)
                    0,    // no price limit
                );
                // Only count as successful if we consumed input
                let output = if amount_in_used > 0 { amount_out } else { 0 };
                (output, PoolSimulator::Clmm(new_state))
            }
            PoolSimulator::Dlmm(state) => {
                // swap_for_y = base_to_quote (swapping X for Y)
                let swap_for_y = base_to_quote;
                let (_total_in, total_out, new_state) =
                    state.simulate_swap(amount_in, swap_for_y);
                (total_out, PoolSimulator::Dlmm(new_state))
            }
        }
    }

    /// Get the fee in basis points
    pub fn fee_bps(&self) -> u16 {
        match self {
            PoolSimulator::ConstantProduct(state) => state.fee_bps,
            PoolSimulator::Clmm(state) => (state.fee_rate / 100) as u16, // fee_rate in millionths
            PoolSimulator::Dlmm(state) => (state.total_fee_rate() / 100_000) as u16, // millionths to bps
        }
    }

    /// Create a ConstantProduct simulator from a VirtualPoolState
    pub fn from_virtual(state: VirtualPoolState) -> Self {
        PoolSimulator::ConstantProduct(state)
    }
}





// =============================================================================
// PURE ARBITRAGE (no trigger transaction - triggered by price drift)
// =============================================================================

/// Result of pure arbitrage simulation (cross-pool price discrepancy)
#[derive(Clone, Debug)]
pub struct PureArbResult {
    /// Profit after flash loan repayment (in borrow token base units)
    pub profit: u64,
    /// Amount to borrow/trade (in borrow token base units - e.g., lamports for SOL)
    pub trade_amount: u64,
    /// Output from leg 1 (in intermediate token base units)
    pub leg1_output: u64,
    /// Output from leg 2 (in borrow token base units - what we get back)
    pub leg2_output: u64,
    /// Flash loan fee (in borrow token base units)
    pub flash_loan_fee: u64,
    /// Direction: true = buy on pool_a/sell on pool_b, false = buy on pool_b/sell on pool_a
    pub direction: bool,
    /// Minimum acceptable leg1 output (with slippage applied)
    pub min_leg1_output: u64,
    /// Minimum acceptable leg2 output (with slippage applied)
    pub min_leg2_output: u64,
}

/// Apply Token-2022 transfer fee to an amount
#[inline]
fn apply_transfer_fee(amount: u64, fee_bps: u16) -> u64 {
    if fee_bps == 0 {
        amount
    } else {
        let fee = (amount as u128 * fee_bps as u128 / 10_000) as u64;
        amount.saturating_sub(fee)
    }
}

/// Simulate pure arbitrage between two pools trading the same pair.
///
/// # Arguments
/// * `pool_a` - First pool simulator
/// * `pool_b` - Second pool simulator  
/// * `trade_amount` - Amount to borrow (in borrow token base units)
/// * `flash_fee_bps` - Flash loan fee in basis points (e.g., 30 = 0.3%)
/// * `borrow_is_base` - true if borrow_mint == pool's base_mint, false if borrow_mint == pool's quote_mint
/// * `slippage_bps` - Slippage tolerance in basis points for min outputs
///
/// # Flow
/// When borrow_is_base=true:  borrow BASE -> swap BASE->QUOTE on buy_pool -> swap QUOTE->BASE on sell_pool -> repay BASE
/// When borrow_is_base=false: borrow QUOTE -> swap QUOTE->BASE on buy_pool -> swap BASE->QUOTE on sell_pool -> repay QUOTE
///
/// Returns the best direction automatically.
pub fn simulate_pure_arb_unified(
    pool_a: &PoolSimulator,
    pool_b: &PoolSimulator,
    trade_amount: u64,
    flash_fee_bps: u16,
    borrow_is_base: bool,
    slippage_bps: u16,
) -> Option<PureArbResult> {
    simulate_pure_arb_unified_with_fees(
        pool_a, pool_b, trade_amount, flash_fee_bps, borrow_is_base, slippage_bps, 0, 0,
    )
}

/// Simulate a pure arbitrage with Token-2022 transfer fees.
///
/// Transfer fees are applied to swap outputs:
/// - `transfer_fee_base_bps` - Fee on base token transfers (applied when receiving base)
/// - `transfer_fee_quote_bps` - Fee on quote token transfers (applied when receiving quote)
pub fn simulate_pure_arb_unified_with_fees(
    pool_a: &PoolSimulator,
    pool_b: &PoolSimulator,
    trade_amount: u64,
    flash_fee_bps: u16,
    borrow_is_base: bool,
    slippage_bps: u16,
    transfer_fee_base_bps: u16,
    transfer_fee_quote_bps: u16,
) -> Option<PureArbResult> {
    // Determine swap directions based on which token we're borrowing
    // If borrow_is_base: leg1 = base->quote (sell base for quote), leg2 = quote->base (buy base with quote)
    // If !borrow_is_base: leg1 = quote->base (buy base with quote), leg2 = base->quote (sell base for quote)
    let (leg1_base_to_quote, leg2_base_to_quote) = if borrow_is_base {
        (true, false)   // borrow base, swap to quote, swap back to base
    } else {
        (false, true)   // borrow quote, swap to base, swap back to quote
    };

    // Determine which transfer fee applies to each leg's output
    // leg1_base_to_quote=true means output is quote, so apply quote fee
    // leg1_base_to_quote=false means output is base, so apply base fee
    let (leg1_fee_bps, leg2_fee_bps) = if borrow_is_base {
        (transfer_fee_quote_bps, transfer_fee_base_bps)  // leg1 outputs quote, leg2 outputs base
    } else {
        (transfer_fee_base_bps, transfer_fee_quote_bps)  // leg1 outputs base, leg2 outputs quote
    };

    let flash_fee = (trade_amount as u128 * flash_fee_bps as u128 / 10_000) as u64;
    let total_cost = trade_amount.saturating_add(flash_fee);
    let slippage_mult = 10_000u64.saturating_sub(slippage_bps as u64);

    // Try direction 1: leg1 on pool_a, leg2 on pool_b
    let dir1_result = {
        let (leg1_out_raw, _) = pool_a.simulate_swap(trade_amount, leg1_base_to_quote);
        if leg1_out_raw == 0 {
            None
        } else {
            // Apply transfer fee to leg1 output
            let leg1_out = apply_transfer_fee(leg1_out_raw, leg1_fee_bps);
            let (leg2_out_raw, _) = pool_b.simulate_swap(leg1_out, leg2_base_to_quote);
            // Apply transfer fee to leg2 output
            let leg2_out = apply_transfer_fee(leg2_out_raw, leg2_fee_bps);
            let profit = leg2_out.saturating_sub(total_cost);
            if profit > 0 {
                Some((profit, leg1_out, leg2_out))
            } else {
                None
            }
        }
    };

    // Try direction 2: leg1 on pool_b, leg2 on pool_a
    let dir2_result = {
        let (leg1_out_raw, _) = pool_b.simulate_swap(trade_amount, leg1_base_to_quote);
        if leg1_out_raw == 0 {
            None
        } else {
            // Apply transfer fee to leg1 output
            let leg1_out = apply_transfer_fee(leg1_out_raw, leg1_fee_bps);
            let (leg2_out_raw, _) = pool_a.simulate_swap(leg1_out, leg2_base_to_quote);
            // Apply transfer fee to leg2 output
            let leg2_out = apply_transfer_fee(leg2_out_raw, leg2_fee_bps);
            let profit = leg2_out.saturating_sub(total_cost);
            if profit > 0 {
                Some((profit, leg1_out, leg2_out))
            } else {
                None
            }
        }
    };

    // Return the more profitable direction
    // Track which pool is used for leg2 to re-simulate with conservative input
    let (profit, leg1_out, leg2_out, direction, leg2_pool) = match (dir1_result, dir2_result) {
        (Some((p1, l1_1, l2_1)), Some((p2, l1_2, l2_2))) => {
            if p1 >= p2 {
                (p1, l1_1, l2_1, true, pool_b)
            } else {
                (p2, l1_2, l2_2, false, pool_a)
            }
        }
        (Some((p, l1, l2)), None) => (p, l1, l2, true, pool_b),
        (None, Some((p, l1, l2))) => (p, l1, l2, false, pool_a),
        (None, None) => return None,
    };

    // Calculate min_leg1_output with slippage
    let min_leg1_output = leg1_out.saturating_mul(slippage_mult) / 10_000;
    
    // CRITICAL: Simulate leg 2 with conservative input (min_leg1_output) to get realistic min_leg2_output
    // This ensures the bundle's leg 2 slippage guard matches what we'll actually execute
    let (conservative_leg2_raw, _) = leg2_pool.simulate_swap(min_leg1_output, leg2_base_to_quote);
    let conservative_leg2_out = apply_transfer_fee(conservative_leg2_raw, leg2_fee_bps);
    let min_leg2_output = conservative_leg2_out.saturating_mul(slippage_mult) / 10_000;

    Some(PureArbResult {
        profit,
        trade_amount,
        leg1_output: leg1_out,
        leg2_output: leg2_out,
        flash_loan_fee: flash_fee,
        direction,
        min_leg1_output,
        min_leg2_output,
    })
}

/// Find optimal trade amount for pure arbitrage using multi-point sampling.
/// 
/// Uses a hybrid approach:
/// 1. Sample multiple points across the range to handle non-unimodal profit curves
///    (CLMM/DLMM pools can have local maxima at tick/bin boundaries)
/// 2. Refine around the best sample point
///
/// # Arguments
/// * `pool_a`, `pool_b` - Pool simulators
/// * `max_trade` - Maximum trade amount (in borrow token base units)
/// * `flash_fee_bps` - Flash loan fee in basis points
/// * `borrow_is_base` - true if borrow_mint == pool's base_mint
/// * `slippage_bps` - Slippage tolerance for min outputs
pub fn find_optimal_pure_arb_unified(
    pool_a: &PoolSimulator,
    pool_b: &PoolSimulator,
    max_trade: u64,
    flash_fee_bps: u16,
    borrow_is_base: bool,
    slippage_bps: u16,
) -> Option<PureArbResult> {
    find_optimal_pure_arb_unified_with_fees(
        pool_a, pool_b, max_trade, flash_fee_bps, borrow_is_base, slippage_bps, 0, 0,
    )
}

/// Find optimal trade amount for pure arbitrage accounting for Token-2022 transfer fees.
///
/// # Arguments
/// * `transfer_fee_base_bps` - Transfer fee on base token (in bps)
/// * `transfer_fee_quote_bps` - Transfer fee on quote token (in bps)
pub fn find_optimal_pure_arb_unified_with_fees(
    pool_a: &PoolSimulator,
    pool_b: &PoolSimulator,
    max_trade: u64,
    flash_fee_bps: u16,
    borrow_is_base: bool,
    slippage_bps: u16,
    transfer_fee_base_bps: u16,
    transfer_fee_quote_bps: u16,
) -> Option<PureArbResult> {
    let min_trade = (max_trade / 1000).max(10_000);
    
    // Phase 1: Coarse sampling across the entire range
    // Sample 20 points to find approximate optimum (handles non-unimodal curves)
    const COARSE_SAMPLES: u64 = 20;
    let step = (max_trade - min_trade) / COARSE_SAMPLES;
    
    let mut best_result: Option<PureArbResult> = None;
    let mut best_profit = 0u64;
    let mut best_amount = min_trade;
    
    if step > 0 {
        for i in 0..=COARSE_SAMPLES {
            let amount = min_trade + i * step;
            if let Some(result) = simulate_pure_arb_unified_with_fees(
                pool_a, pool_b, amount, flash_fee_bps, borrow_is_base, slippage_bps,
                transfer_fee_base_bps, transfer_fee_quote_bps,
            ) {
                if result.profit > best_profit {
                    best_profit = result.profit;
                    best_amount = amount;
                    best_result = Some(result);
                }
            }
        }
    }
    
    // Phase 2: Fine-grained search around the best point
    // Search +/- 2 step sizes with finer granularity
    let fine_range = step.max(max_trade / 50);
    let fine_step = fine_range / 20;
    
    if fine_step > 0 {
        let fine_low = best_amount.saturating_sub(fine_range).max(min_trade);
        let fine_high = (best_amount + fine_range).min(max_trade);
        
        let mut amount = fine_low;
        while amount <= fine_high {
            if let Some(result) = simulate_pure_arb_unified_with_fees(
                pool_a, pool_b, amount, flash_fee_bps, borrow_is_base, slippage_bps,
                transfer_fee_base_bps, transfer_fee_quote_bps,
            ) {
                if result.profit > best_profit {
                    best_profit = result.profit;
                    best_result = Some(result);
                }
            }
            amount += fine_step;
        }
    }
    
    // Phase 3: Also check a few "strategic" amounts that might hit tick/bin boundaries
    // For CLMM/DLMM, round numbers often align better with liquidity
    let strategic_amounts = [
        max_trade / 100,      // 1%
        max_trade / 50,       // 2%
        max_trade / 20,       // 5%
        max_trade / 10,       // 10%
    ];
    
    for amount in strategic_amounts {
        if amount >= min_trade && amount <= max_trade {
            if let Some(result) = simulate_pure_arb_unified_with_fees(
                pool_a, pool_b, amount, flash_fee_bps, borrow_is_base, slippage_bps,
                transfer_fee_base_bps, transfer_fee_quote_bps,
            ) {
                if result.profit > best_profit {
                    best_profit = result.profit;
                    best_result = Some(result);
                }
            }
        }
    }
    
    best_result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_pool_swap() {
        // Pool: 1000 SOL, 200,000 USDC
        let pool = VirtualPoolState::new(
            1_000_000_000_000, // 1000 SOL
            200_000_000_000,   // 200,000 USDC
            30,                // 0.3% fee
        );

        // Swap 10 SOL for USDC
        let (output, new_state) = pool.simulate_swap(10_000_000_000, true);

        // Expected: ~1980 USDC (fee + slippage)
        assert!(
            output > 1_900_000_000 && output < 2_000_000_000,
            "output was {}",
            output
        );

        // Reserves should update with input_after_fee
        // Input: 10 SOL, fee: 0.3% = 30M lamports, so input_after_fee = 9.97 SOL
        let expected_fee = 10_000_000_000u64 * 30 / 10_000; // 30M
        let input_after_fee = 10_000_000_000u64 - expected_fee;
        assert_eq!(new_state.base_reserve, 1_000_000_000_000 + input_after_fee);
        assert!(new_state.quote_reserve < 200_000_000_000);
    }



    #[test]
    fn test_constant_product_small_trade() {
        // Pool: 1000 SOL, 200,000 USDC (price = 200 USDC/SOL)
        let sol_reserve = 1_000_000_000_000u64; // 1000 SOL in lamports
        let usdc_reserve = 200_000_000_000u64; // 200,000 USDC in base units

        // Trade 1 SOL for USDC
        let input = 1_000_000_000u64; // 1 SOL
        let (output, impact) = calculate_constant_product(input, sol_reserve, usdc_reserve);

        // Expected: ~199.8 USDC (small slippage)
        // Spot price: 200 USDC/SOL
        // Actual: 200k * 1 / (1000 + 1) = 199.8 USDC
        assert!(output > 199_000_000 && output < 200_000_000);
        assert!(impact < 20); // Less than 0.2% impact
    }

    #[test]
    fn test_constant_product_large_trade() {
        // Pool: 1000 SOL, 200,000 USDC
        let sol_reserve = 1_000_000_000_000u64;
        let usdc_reserve = 200_000_000_000u64;

        // Trade 100 SOL (10% of pool)
        let input = 100_000_000_000u64;
        let (output, impact) = calculate_constant_product(input, sol_reserve, usdc_reserve);

        // Expected: ~18,182 USDC (significant slippage)
        // Spot would give: 20,000 USDC
        // Actual: 200k * 100 / (1000 + 100) = 18,182 USDC
        assert!(output > 18_000_000_000 && output < 19_000_000_000);
        assert!(impact > 800 && impact < 1000); // ~9% impact
    }

    #[test]
    fn test_max_input_for_slippage() {
        let input_reserve = 1_000_000_000_000u64; // 1000 SOL
        let output_reserve = 200_000_000_000u64;

        // Max trade for 1% slippage
        let max_input = max_input_for_slippage(input_reserve, output_reserve, 100);

        // Should be about 1% of reserve / (1 - 0.01) ≈ 10.1 SOL
        assert!(max_input > 10_000_000_000 && max_input < 11_000_000_000);
    }
}
