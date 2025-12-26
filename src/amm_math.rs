//! AMM math for calculating expected swap outputs with slippage.
//!
//! Supports multiple curve types:
//! - Constant Product (x * y = k): Raydium AMM, Orca Legacy, Meteora DAMM
//! - Concentrated Liquidity (tick-based): Raydium CLMM, Orca Whirlpool, PancakeSwap CLMM
//! - Discrete Liquidity (bin-based): Meteora DLMM

use crate::clmm_math::ClmmPoolState;
use crate::dlmm_math::DlmmPoolState;

/// Curve types with their swap math
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveType {
    /// x * y = k (Raydium AMM, Orca Legacy, Meteora DAMM)
    ConstantProduct,
    /// Concentrated liquidity (Raydium CLMM, Orca Whirlpool, PancakeSwap CLMM)
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
    let numerator = out_reserve_128 * input_128;
    let denominator = in_reserve_128 + input_128;
    let output_amount = (numerator / denominator) as u64;

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
/// Used to calculate arb profit by simulating victim's swap then our arb legs.
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
    /// Flash loan fee
    pub flash_loan_fee: u64,
    /// Is this a cross-pool arb (true) or sandwich (false)?
    pub is_cross_pool: bool,
}

/// Simulate a cross-pool backrun arbitrage
///
/// After victim swaps A→B on Pool X:
/// 1. Leg 1: Swap A→B on Pool Y (unaffected, B is cheap here)
/// 2. Leg 2: Swap B→A on Pool X (post-victim, B is expensive here)
///
/// Profit = leg2_output - borrow_amount - flash_fee
pub fn simulate_cross_pool_backrun(
    pool_x: &VirtualPoolState,  // Victim's pool
    pool_y: &VirtualPoolState,  // Alternate pool
    victim_amount: u64,         // Victim's input amount
    victim_base_to_quote: bool, // Victim's swap direction
    borrow_amount: u64,         // How much we borrow
    borrow_is_base: bool,       // Are we borrowing base token?
    flash_fee_bps: u16,         // Flash loan fee (typically 30 = 0.3%)
) -> Option<ArbSimulationResult> {
    // 1. Simulate victim's swap on Pool X
    let (_, pool_x_after_victim) = pool_x.simulate_swap(victim_amount, victim_base_to_quote);

    // 2. Our leg 1: Swap on Pool Y (unaffected by victim)
    //    We swap borrow_token → intermediate_token
    //    Direction depends on what we borrowed
    let leg1_base_to_quote = borrow_is_base;
    let (leg1_output, _) = pool_y.simulate_swap(borrow_amount, leg1_base_to_quote);

    if leg1_output == 0 {
        return None;
    }

    // 3. Our leg 2: Swap intermediate → borrow on Pool X (post-victim)
    //    Opposite direction from leg 1
    let leg2_base_to_quote = !borrow_is_base;
    let (leg2_output, _) = pool_x_after_victim.simulate_swap(leg1_output, leg2_base_to_quote);

    if leg2_output == 0 {
        return None;
    }

    // 4. Calculate profit
    let flash_fee = (borrow_amount as u128 * flash_fee_bps as u128 / 10_000) as u64;
    let total_cost = borrow_amount.saturating_add(flash_fee);

    if leg2_output > total_cost {
        Some(ArbSimulationResult {
            profit: leg2_output - total_cost,
            borrow_amount,
            leg1_output,
            leg2_output,
            flash_loan_fee: flash_fee,
            is_cross_pool: true,
        })
    } else {
        None
    }
}

/// Simulate a sandwich attack (front-run + back-run on same pool)
///
/// 1. Front-run: We swap same direction as victim (before them)
/// 2. Victim swaps (gets worse price due to our front-run)
/// 3. Back-run: We swap opposite direction (after victim)
///
/// Profit = backrun_output - frontrun_input - flash_fee
pub fn simulate_sandwich(
    pool: &VirtualPoolState,
    victim_amount: u64,
    victim_base_to_quote: bool,
    our_frontrun_amount: u64,
    flash_fee_bps: u16,
) -> Option<ArbSimulationResult> {
    // 1. Our front-run (same direction as victim)
    let (frontrun_output, pool_after_frontrun) =
        pool.simulate_swap(our_frontrun_amount, victim_base_to_quote);

    if frontrun_output == 0 {
        return None;
    }

    // 2. Victim's swap (worse price now)
    let (victim_output, pool_after_victim) =
        pool_after_frontrun.simulate_swap(victim_amount, victim_base_to_quote);

    // Victim must still get at least their min_out (we'd check this separately)
    if victim_output == 0 {
        return None;
    }

    // 3. Our back-run (opposite direction)
    let (backrun_output, _) =
        pool_after_victim.simulate_swap(frontrun_output, !victim_base_to_quote);

    if backrun_output == 0 {
        return None;
    }

    // 4. Calculate profit
    let flash_fee = (our_frontrun_amount as u128 * flash_fee_bps as u128 / 10_000) as u64;
    let total_cost = our_frontrun_amount.saturating_add(flash_fee);

    if backrun_output > total_cost {
        Some(ArbSimulationResult {
            profit: backrun_output - total_cost,
            borrow_amount: our_frontrun_amount,
            leg1_output: frontrun_output,
            leg2_output: backrun_output,
            flash_loan_fee: flash_fee,
            is_cross_pool: false,
        })
    } else {
        None
    }
}

/// Find optimal borrow amount for cross-pool arbitrage using binary search
/// Returns (optimal_amount, expected_profit)
pub fn find_optimal_cross_pool_borrow(
    pool_x: &VirtualPoolState,
    pool_y: &VirtualPoolState,
    victim_amount: u64,
    victim_base_to_quote: bool,
    borrow_is_base: bool,
    max_borrow: u64,
    flash_fee_bps: u16,
) -> (u64, u64) {
    let mut best_profit = 0u64;
    let mut best_amount = 0u64;

    // Binary search for optimal amount
    let mut low = max_borrow / 1000; // Start at 0.1% of max
    let mut high = max_borrow;

    // If low is too small, set a minimum
    low = low.max(10_000); // At least 10k lamports

    while high > low + 10_000 {
        // Precision of ~0.00001 SOL
        let mid = (low + high) / 2;

        let profit_at_mid = simulate_cross_pool_backrun(
            pool_x,
            pool_y,
            victim_amount,
            victim_base_to_quote,
            mid,
            borrow_is_base,
            flash_fee_bps,
        )
        .map(|r| r.profit)
        .unwrap_or(0);

        let profit_at_mid_plus = simulate_cross_pool_backrun(
            pool_x,
            pool_y,
            victim_amount,
            victim_base_to_quote,
            mid + 10_000,
            borrow_is_base,
            flash_fee_bps,
        )
        .map(|r| r.profit)
        .unwrap_or(0);

        if profit_at_mid_plus > profit_at_mid {
            low = mid;
        } else {
            high = mid;
        }

        if profit_at_mid > best_profit {
            best_profit = profit_at_mid;
            best_amount = mid;
        }
    }

    // Check final value
    if let Some(result) = simulate_cross_pool_backrun(
        pool_x,
        pool_y,
        victim_amount,
        victim_base_to_quote,
        best_amount,
        borrow_is_base,
        flash_fee_bps,
    ) {
        (best_amount, result.profit)
    } else {
        (0, 0)
    }
}

/// Find optimal amount for sandwich attack using binary search
pub fn find_optimal_sandwich_amount(
    pool: &VirtualPoolState,
    victim_amount: u64,
    victim_base_to_quote: bool,
    victim_min_output: u64, // Must ensure victim still gets at least this
    max_borrow: u64,
    flash_fee_bps: u16,
) -> (u64, u64) {
    let mut best_profit = 0u64;
    let mut best_amount = 0u64;

    // Binary search
    let mut low = max_borrow / 1000;
    low = low.max(10_000);
    let mut high = max_borrow;

    while high > low + 10_000 {
        let mid = (low + high) / 2;

        // Simulate and check if victim still gets min_output
        let valid_and_profit = {
            let (frontrun_out, pool_after_fr) = pool.simulate_swap(mid, victim_base_to_quote);
            let (victim_out, pool_after_v) =
                pool_after_fr.simulate_swap(victim_amount, victim_base_to_quote);

            // Victim must get at least their min_output
            if victim_out < victim_min_output {
                None
            } else {
                let (backrun_out, _) =
                    pool_after_v.simulate_swap(frontrun_out, !victim_base_to_quote);
                let flash_fee = (mid as u128 * flash_fee_bps as u128 / 10_000) as u64;
                if backrun_out > mid + flash_fee {
                    Some(backrun_out - mid - flash_fee)
                } else {
                    Some(0)
                }
            }
        };

        let profit_at_mid = valid_and_profit.unwrap_or(0);

        // Check slightly higher
        let profit_at_mid_plus = {
            let test_amount = mid + 10_000;
            let (frontrun_out, pool_after_fr) =
                pool.simulate_swap(test_amount, victim_base_to_quote);
            let (victim_out, pool_after_v) =
                pool_after_fr.simulate_swap(victim_amount, victim_base_to_quote);

            if victim_out < victim_min_output {
                0
            } else {
                let (backrun_out, _) =
                    pool_after_v.simulate_swap(frontrun_out, !victim_base_to_quote);
                let flash_fee = (test_amount as u128 * flash_fee_bps as u128 / 10_000) as u64;
                if backrun_out > test_amount + flash_fee {
                    backrun_out - test_amount - flash_fee
                } else {
                    0
                }
            }
        };

        if profit_at_mid_plus > profit_at_mid {
            low = mid;
        } else {
            high = mid;
        }

        if profit_at_mid > best_profit {
            best_profit = profit_at_mid;
            best_amount = mid;
        }
    }

    (best_amount, best_profit)
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
    /// Constant product AMM (Raydium AMM, Meteora DAMM)
    ConstantProduct(VirtualPoolState),
    /// Concentrated liquidity (Raydium CLMM, Orca Whirlpool)
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

    /// Create from VirtualPoolState (for backwards compatibility)
    pub fn from_virtual(state: VirtualPoolState) -> Self {
        PoolSimulator::ConstantProduct(state)
    }
}

/// Simulate cross-pool backrun using unified PoolSimulator.
///
/// This version works with any pool type (AMM, CLMM, DLMM).
pub fn simulate_cross_pool_backrun_unified(
    pool_x: &PoolSimulator,       // Victim's pool
    pool_y: &PoolSimulator,       // Alternate pool
    victim_amount: u64,           // Victim's input amount
    victim_base_to_quote: bool,   // Victim's swap direction
    borrow_amount: u64,           // How much we borrow
    borrow_is_base: bool,         // Are we borrowing base token?
    flash_fee_bps: u16,           // Flash loan fee (typically 30 = 0.3%)
) -> Option<ArbSimulationResult> {
    // 1. Simulate victim's swap on Pool X
    let (_, pool_x_after_victim) = pool_x.simulate_swap(victim_amount, victim_base_to_quote);

    // 2. Our leg 1: Swap on Pool Y (unaffected by victim)
    //    We swap borrow_token → intermediate_token
    let leg1_base_to_quote = borrow_is_base;
    let (leg1_output, _) = pool_y.simulate_swap(borrow_amount, leg1_base_to_quote);

    if leg1_output == 0 {
        return None;
    }

    // 3. Our leg 2: Swap intermediate → borrow on Pool X (post-victim)
    let leg2_base_to_quote = !borrow_is_base;
    let (leg2_output, _) = pool_x_after_victim.simulate_swap(leg1_output, leg2_base_to_quote);

    if leg2_output == 0 {
        return None;
    }

    // 4. Calculate profit
    let flash_fee = (borrow_amount as u128 * flash_fee_bps as u128 / 10_000) as u64;
    let total_cost = borrow_amount.saturating_add(flash_fee);

    if leg2_output > total_cost {
        Some(ArbSimulationResult {
            profit: leg2_output - total_cost,
            borrow_amount,
            leg1_output,
            leg2_output,
            flash_loan_fee: flash_fee,
            is_cross_pool: true,
        })
    } else {
        None
    }
}

/// Find optimal borrow amount using unified PoolSimulator.
pub fn find_optimal_borrow_unified(
    pool_x: &PoolSimulator,
    pool_y: &PoolSimulator,
    victim_amount: u64,
    victim_base_to_quote: bool,
    borrow_is_base: bool,
    max_borrow: u64,
    flash_fee_bps: u16,
) -> (u64, u64) {
    let mut best_profit = 0u64;
    let mut best_amount = 0u64;

    // Binary search for optimal amount
    let mut low = max_borrow / 1000;
    let mut high = max_borrow;
    low = low.max(10_000); // At least 10k lamports

    while high > low + 10_000 {
        let mid = (low + high) / 2;

        let profit_at_mid = simulate_cross_pool_backrun_unified(
            pool_x,
            pool_y,
            victim_amount,
            victim_base_to_quote,
            mid,
            borrow_is_base,
            flash_fee_bps,
        )
        .map(|r| r.profit)
        .unwrap_or(0);

        let profit_at_mid_plus = simulate_cross_pool_backrun_unified(
            pool_x,
            pool_y,
            victim_amount,
            victim_base_to_quote,
            mid + 10_000,
            borrow_is_base,
            flash_fee_bps,
        )
        .map(|r| r.profit)
        .unwrap_or(0);

        if profit_at_mid_plus > profit_at_mid {
            low = mid;
        } else {
            high = mid;
        }

        if profit_at_mid > best_profit {
            best_profit = profit_at_mid;
            best_amount = mid;
        }
    }

    // Check final value
    if let Some(result) = simulate_cross_pool_backrun_unified(
        pool_x,
        pool_y,
        victim_amount,
        victim_base_to_quote,
        best_amount,
        borrow_is_base,
        flash_fee_bps,
    ) {
        (best_amount, result.profit)
    } else {
        (0, 0)
    }
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
    fn test_sandwich_simulation() {
        let pool = VirtualPoolState::new(
            1_000_000_000_000, // 1000 SOL
            200_000_000_000,   // 200,000 USDC
            30,
        );

        // Victim wants to swap 10 SOL
        let result = simulate_sandwich(
            &pool,
            10_000_000_000, // victim amount
            true,           // base to quote
            5_000_000_000,  // our frontrun: 5 SOL
            30,             // flash fee
        );

        // Should be profitable (victim's slippage is our profit)
        if let Some(r) = result {
            println!("Sandwich profit: {} lamports", r.profit);
            assert!(r.profit > 0, "Sandwich should be profitable");
        }
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
