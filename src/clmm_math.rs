//! Concentrated Liquidity Market Maker (CLMM) Math
//!
//! Implements tick-based swap simulation for:
//! - Raydium CLMM (raydium-io/raydium-clmm)
//! - Orca Whirlpool (orca-so/whirlpools)
//!
//! # Reference Formulas (from Raydium CLMM)
//!
//! ## Tick ↔ Price Conversion
//! - `sqrt_price = 1.0001^(tick/2)` in Q64.64 fixed-point
//! - Stored as `sqrt_price_x64 = sqrt_price * 2^64`
//!
//! ## Token Delta Formulas
//! - `Δa = L * (1/√P_lower - 1/√P_upper)` (token A delta)
//! - `Δb = L * (√P_upper - √P_lower)` (token B delta)
//!
//! ## Next Price from Input
//! - For token A input: `√P' = √P * L / (L + Δa * √P)`
//! - For token B input: `√P' = √P + Δb / L`

// =============================================================================
// CONSTANTS (from Raydium CLMM)
// =============================================================================

/// Q64.64 fixed-point scale factor (2^64)
pub const Q64: u128 = 1u128 << 64;

/// Minimum sqrt price (tick = -443636)
pub const MIN_SQRT_PRICE_X64: u128 = 4295048016;

/// Maximum sqrt price (tick = 443636)
pub const MAX_SQRT_PRICE_X64: u128 = 79226673521066979257578248091;

/// Minimum tick
pub const MIN_TICK: i32 = -443636;

/// Maximum tick
pub const MAX_TICK: i32 = 443636;

/// Fee rate denominator (1_000_000 = 100%)
pub const FEE_RATE_DENOMINATOR: u32 = 1_000_000;

// =============================================================================
// TICK MATH (from raydium-clmm/programs/amm/src/libraries/tick_math.rs)
// =============================================================================

/// Get sqrt price at a given tick.
/// Formula: sqrt_price = 1.0001^(tick/2) in Q64.64
///
/// Reference: raydium-io/raydium-clmm tick_math::get_sqrt_price_at_tick
///
/// NOTE: The original implementation uses Q128.128 intermediate arithmetic.
/// We use a simplified approach with 128-bit multiplication and proper handling.
pub fn get_sqrt_price_at_tick(tick: i32) -> Option<u128> {
    if tick < MIN_TICK || tick > MAX_TICK {
        return None;
    }

    // Use floating-point for intermediate calculation, then convert to Q64.64
    // sqrt_price = 1.0001^(tick/2) = sqrt(1.0001^tick)
    // In Q64.64: result = sqrt(1.0001^tick) * 2^64
    let base: f64 = 1.0001_f64;
    let exponent = tick as f64 / 2.0;
    let sqrt_price = base.powf(exponent);

    // Convert to Q64.64 fixed point
    let q64 = (1u128 << 64) as f64;
    let result = (sqrt_price * q64) as u128;

    // Clamp to valid range
    if result < MIN_SQRT_PRICE_X64 {
        return Some(MIN_SQRT_PRICE_X64);
    }
    if result > MAX_SQRT_PRICE_X64 {
        return Some(MAX_SQRT_PRICE_X64);
    }

    Some(result)
}

/// Get tick at a given sqrt price.
/// Formula: tick = log_{1.0001}(price) = 2 * log_{1.0001}(sqrt_price)
///
/// Reference: raydium-io/raydium-clmm tick_math::get_tick_at_sqrt_price
pub fn get_tick_at_sqrt_price(sqrt_price_x64: u128) -> Option<i32> {
    if sqrt_price_x64 < MIN_SQRT_PRICE_X64 || sqrt_price_x64 > MAX_SQRT_PRICE_X64 {
        return None;
    }

    // Binary search for the tick
    let mut low = MIN_TICK;
    let mut high = MAX_TICK;

    while low < high {
        let mid = low + (high - low) / 2;
        let price_at_mid = get_sqrt_price_at_tick(mid)?;

        if price_at_mid <= sqrt_price_x64 {
            low = mid + 1;
        } else {
            high = mid;
        }
    }

    Some(low - 1)
}

// =============================================================================
// LIQUIDITY MATH (from raydium-clmm/programs/amm/src/libraries/liquidity_math.rs)
// =============================================================================

/// Get delta amount_a for given liquidity and price range.
/// Formula: Δa = L * (1/√P_lower - 1/√P_upper)
///        = L * (√P_upper - √P_lower) / (√P_upper * √P_lower)
///
/// Reference: raydium-io/raydium-clmm liquidity_math::get_delta_amount_0_unsigned
pub fn get_delta_amount_a(
    sqrt_price_lower_x64: u128,
    sqrt_price_upper_x64: u128,
    liquidity: u128,
    round_up: bool,
) -> Option<u64> {
    if sqrt_price_lower_x64 > sqrt_price_upper_x64 {
        return None;
    }
    if liquidity == 0 {
        return Some(0);
    }

    let numerator =
        (liquidity as u128).checked_mul(sqrt_price_upper_x64.checked_sub(sqrt_price_lower_x64)?)?;

    let denominator = (sqrt_price_lower_x64 >> 32).checked_mul(sqrt_price_upper_x64 >> 32)?;

    if denominator == 0 {
        return None;
    }

    let result = if round_up {
        numerator.checked_add(denominator.checked_sub(1)?)? / denominator
    } else {
        numerator / denominator
    };

    // Adjust for the shift we did
    let result = result >> 64;

    if result > u64::MAX as u128 {
        Some(u64::MAX)
    } else {
        Some(result as u64)
    }
}

/// Get delta amount_b for given liquidity and price range.
/// Formula: Δb = L * (√P_upper - √P_lower)
///
/// Reference: raydium-io/raydium-clmm liquidity_math::get_delta_amount_1_unsigned
pub fn get_delta_amount_b(
    sqrt_price_lower_x64: u128,
    sqrt_price_upper_x64: u128,
    liquidity: u128,
    round_up: bool,
) -> Option<u64> {
    if sqrt_price_lower_x64 > sqrt_price_upper_x64 {
        return None;
    }
    if liquidity == 0 {
        return Some(0);
    }

    let delta_price = sqrt_price_upper_x64.checked_sub(sqrt_price_lower_x64)?;
    let product = (liquidity as u128).checked_mul(delta_price)?;

    let result = if round_up {
        (product + Q64 - 1) / Q64
    } else {
        product / Q64
    };

    if result > u64::MAX as u128 {
        Some(u64::MAX)
    } else {
        Some(result as u64)
    }
}

// =============================================================================
// SQRT PRICE MATH (from raydium-clmm/programs/amm/src/libraries/sqrt_price_math.rs)
// =============================================================================

/// Get next sqrt price from token A input.
/// Formula: √P' = √P * L / (L + Δa * √P)
///
/// Reference: raydium-io/raydium-clmm sqrt_price_math::get_next_sqrt_price_from_input (zero_for_one=true)
pub fn get_next_sqrt_price_a_up(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount: u64,
    _round_up: bool,
) -> Option<u128> {
    if amount == 0 || liquidity == 0 {
        return Some(sqrt_price_x64);
    }

    // product = amount * sqrt_price
    let product = (amount as u128).checked_mul(sqrt_price_x64)?;

    // denominator = L + product
    let liquidity_shifted = liquidity.checked_mul(Q64)?;
    let denominator = liquidity_shifted.checked_add(product)?;

    // numerator = L * sqrt_price
    let numerator = liquidity_shifted.checked_mul(sqrt_price_x64)?;

    // result = numerator / denominator (with rounding up)
    let result = numerator.checked_add(denominator.checked_sub(1)?)? / denominator;

    Some(result)
}

/// Get next sqrt price from token B input.
/// Formula: √P' = √P + Δb / L
///
/// Reference: raydium-io/raydium-clmm sqrt_price_math::get_next_sqrt_price_from_input (zero_for_one=false)
pub fn get_next_sqrt_price_b_down(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount: u64,
) -> Option<u128> {
    if amount == 0 || liquidity == 0 {
        return Some(sqrt_price_x64);
    }

    // quotient = amount * Q64 / liquidity
    let quotient = ((amount as u128).checked_mul(Q64)?) / liquidity;

    sqrt_price_x64.checked_add(quotient)
}

// =============================================================================
// SWAP MATH (from raydium-clmm/programs/amm/src/libraries/swap_math.rs)
// =============================================================================

/// Result of computing a single swap step within one tick range.
#[derive(Debug, Clone, Default)]
pub struct SwapStepResult {
    /// The sqrt price after the swap step
    pub sqrt_price_next_x64: u128,
    /// Amount of input token consumed in this step
    pub amount_in: u64,
    /// Amount of output token produced in this step
    pub amount_out: u64,
    /// Fee amount in input token
    pub fee_amount: u64,
}

/// Compute the result of swapping within a single tick range.
///
/// This is the core CLMM swap calculation for one step between ticks.
///
/// Reference: raydium-io/raydium-clmm swap_math::compute_swap_step
pub fn compute_swap_step(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    amount_remaining: u64,
    fee_rate: u32, // in millionths (e.g., 3000 = 0.3%)
    is_base_input: bool,
    zero_for_one: bool,
) -> Option<SwapStepResult> {
    let mut result = SwapStepResult::default();

    if is_base_input {
        // Exact input case: deduct fee first
        let amount_remaining_less_fee = (amount_remaining as u128)
            .checked_mul((FEE_RATE_DENOMINATOR - fee_rate) as u128)?
            .checked_div(FEE_RATE_DENOMINATOR as u128)?
            as u64;

        // Calculate max amount that can be swapped in this range
        let max_amount_in = if zero_for_one {
            get_delta_amount_a(
                sqrt_price_target_x64,
                sqrt_price_current_x64,
                liquidity,
                true,
            )?
        } else {
            get_delta_amount_b(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                true,
            )?
        };

        if amount_remaining_less_fee >= max_amount_in {
            // We reach the target price
            result.sqrt_price_next_x64 = sqrt_price_target_x64;
            result.amount_in = max_amount_in;
        } else {
            // We don't reach the target price
            result.sqrt_price_next_x64 = if zero_for_one {
                get_next_sqrt_price_a_up(
                    sqrt_price_current_x64,
                    liquidity,
                    amount_remaining_less_fee,
                    true,
                )?
            } else {
                get_next_sqrt_price_b_down(
                    sqrt_price_current_x64,
                    liquidity,
                    amount_remaining_less_fee,
                )?
            };
            result.amount_in = amount_remaining_less_fee;
        }

        // Calculate output
        result.amount_out = if zero_for_one {
            get_delta_amount_b(
                result.sqrt_price_next_x64,
                sqrt_price_current_x64,
                liquidity,
                false,
            )?
        } else {
            get_delta_amount_a(
                sqrt_price_current_x64,
                result.sqrt_price_next_x64,
                liquidity,
                false,
            )?
        };

        // Calculate fee
        if result.sqrt_price_next_x64 != sqrt_price_target_x64 {
            // Didn't reach target, remainder is fee
            result.fee_amount = amount_remaining.saturating_sub(result.amount_in);
        } else {
            result.fee_amount = ((result.amount_in as u128)
                .checked_mul(fee_rate as u128)?
                .checked_div((FEE_RATE_DENOMINATOR - fee_rate) as u128)?)
                as u64;
        }
    } else {
        // Exact output case
        let max_amount_out = if zero_for_one {
            get_delta_amount_b(
                sqrt_price_target_x64,
                sqrt_price_current_x64,
                liquidity,
                false,
            )?
        } else {
            get_delta_amount_a(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                false,
            )?
        };

        if amount_remaining >= max_amount_out {
            result.sqrt_price_next_x64 = sqrt_price_target_x64;
            result.amount_out = max_amount_out;
        } else {
            result.sqrt_price_next_x64 = sqrt_price_current_x64; // Would need inverse calculation
            result.amount_out = amount_remaining;
        }

        // Calculate input needed
        result.amount_in = if zero_for_one {
            get_delta_amount_a(
                result.sqrt_price_next_x64,
                sqrt_price_current_x64,
                liquidity,
                true,
            )?
        } else {
            get_delta_amount_b(
                sqrt_price_current_x64,
                result.sqrt_price_next_x64,
                liquidity,
                true,
            )?
        };

        result.fee_amount = ((result.amount_in as u128)
            .checked_mul(fee_rate as u128)?
            .checked_div((FEE_RATE_DENOMINATOR - fee_rate) as u128)?)
            as u64;
    }

    Some(result)
}

// =============================================================================
// TICK STATE
// =============================================================================

/// Represents a tick with its liquidity delta.
#[derive(Debug, Clone, Copy)]
pub struct TickInfo {
    /// Tick index
    pub tick: i32,
    /// Net liquidity change when crossing this tick (can be negative)
    pub liquidity_net: i128,
    /// Whether this tick is initialized
    pub initialized: bool,
}

// =============================================================================
// CLMM POOL STATE & SWAP SIMULATION
// =============================================================================

/// CLMM pool state for simulation.
///
/// Contains all necessary state to simulate swaps off-chain.
#[derive(Debug, Clone)]
pub struct ClmmPoolState {
    /// Current sqrt price in Q64.64 format
    pub sqrt_price_x64: u128,
    /// Current tick
    pub tick: i32,
    /// Tick spacing (e.g., 1, 10, 60, 200)
    pub tick_spacing: u16,
    /// Current liquidity in active range
    pub liquidity: u128,
    /// Fee rate in millionths (e.g., 3000 = 0.3%)
    pub fee_rate: u32,
    /// Initialized ticks with their liquidity_net values
    /// In production, fetch from on-chain tick arrays
    pub ticks: Vec<TickInfo>,
}

impl ClmmPoolState {
    /// Create a new CLMM pool state.
    pub fn new(
        sqrt_price_x64: u128,
        tick: i32,
        tick_spacing: u16,
        liquidity: u128,
        fee_rate: u32,
    ) -> Self {
        Self {
            sqrt_price_x64,
            tick,
            tick_spacing,
            liquidity,
            fee_rate,
            ticks: Vec::new(),
        }
    }

    /// Add an initialized tick.
    pub fn add_tick(&mut self, tick: i32, liquidity_net: i128) {
        self.ticks.push(TickInfo {
            tick,
            liquidity_net,
            initialized: true,
        });
        self.ticks.sort_by_key(|t| t.tick);
    }

    /// Find the next initialized tick in the swap direction.
    fn next_initialized_tick(&self, current_tick: i32, zero_for_one: bool) -> Option<TickInfo> {
        if zero_for_one {
            // Swapping token0 for token1, price decreasing, find tick <= current
            self.ticks
                .iter()
                .rev()
                .find(|t| t.tick <= current_tick)
                .copied()
        } else {
            // Swapping token1 for token0, price increasing, find tick > current
            self.ticks.iter().find(|t| t.tick > current_tick).copied()
        }
    }

    /// Simulate a swap on this CLMM pool.
    ///
    /// This implements the tick-by-tick iteration from:
    /// - raydium-io/raydium-clmm swap.rs::swap_internal
    /// - orca-so/whirlpools swap.rs::compute_swap
    ///
    /// # Arguments
    /// * `amount_specified` - Input amount (for is_base_input=true) or output amount
    /// * `zero_for_one` - true = swap token0→token1 (price decreases), false = token1→token0
    /// * `is_base_input` - true = exact input, false = exact output
    /// * `sqrt_price_limit_x64` - Price limit (0 for no limit)
    ///
    /// # Returns
    /// * `(amount_in, amount_out, new_state)` - Amounts and updated pool state
    pub fn simulate_swap(
        &self,
        amount_specified: u64,
        zero_for_one: bool,
        is_base_input: bool,
        sqrt_price_limit_x64: u128,
    ) -> (u64, u64, ClmmPoolState) {
        let sqrt_price_limit = if sqrt_price_limit_x64 == 0 {
            if zero_for_one {
                MIN_SQRT_PRICE_X64 + 1
            } else {
                MAX_SQRT_PRICE_X64 - 1
            }
        } else {
            sqrt_price_limit_x64
        };

        // Validate price limit direction
        if zero_for_one && sqrt_price_limit >= self.sqrt_price_x64 {
            return (0, 0, self.clone());
        }
        if !zero_for_one && sqrt_price_limit <= self.sqrt_price_x64 {
            return (0, 0, self.clone());
        }

        let mut state = SwapState {
            amount_specified_remaining: amount_specified,
            amount_calculated: 0,
            sqrt_price_x64: self.sqrt_price_x64,
            tick: self.tick,
            liquidity: self.liquidity,
        };

        // Main swap loop - iterate through tick ranges
        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 100; // Safety limit

        while state.amount_specified_remaining > 0
            && state.sqrt_price_x64 != sqrt_price_limit
            && iterations < MAX_ITERATIONS
        {
            iterations += 1;

            // Find next initialized tick
            let next_tick_info = self.next_initialized_tick(state.tick, zero_for_one);

            let (tick_next, initialized) = match next_tick_info {
                Some(info) => (info.tick, info.initialized),
                None => {
                    // No more initialized ticks, use boundary
                    let boundary = if zero_for_one { MIN_TICK } else { MAX_TICK };
                    (boundary, false)
                }
            };

            // Clamp to valid range
            let tick_next = tick_next.max(MIN_TICK).min(MAX_TICK);

            // Get sqrt price at next tick
            let sqrt_price_next_x64 = match get_sqrt_price_at_tick(tick_next) {
                Some(p) => p,
                None => break,
            };

            // Determine target price (either next tick or limit)
            let sqrt_price_target_x64 = if zero_for_one {
                sqrt_price_next_x64.max(sqrt_price_limit)
            } else {
                sqrt_price_next_x64.min(sqrt_price_limit)
            };

            // Compute swap step within this tick range
            let step_result = match compute_swap_step(
                state.sqrt_price_x64,
                sqrt_price_target_x64,
                state.liquidity,
                state.amount_specified_remaining,
                self.fee_rate,
                is_base_input,
                zero_for_one,
            ) {
                Some(r) => r,
                None => break,
            };

            // Update state
            state.sqrt_price_x64 = step_result.sqrt_price_next_x64;

            if is_base_input {
                state.amount_specified_remaining = state
                    .amount_specified_remaining
                    .saturating_sub(step_result.amount_in + step_result.fee_amount);
                state.amount_calculated = state
                    .amount_calculated
                    .saturating_add(step_result.amount_out);
            } else {
                state.amount_specified_remaining = state
                    .amount_specified_remaining
                    .saturating_sub(step_result.amount_out);
                state.amount_calculated = state
                    .amount_calculated
                    .saturating_add(step_result.amount_in + step_result.fee_amount);
            }

            // If we reached the next tick, cross it and update liquidity
            if state.sqrt_price_x64 == sqrt_price_next_x64 && initialized {
                if let Some(tick_info) = next_tick_info {
                    let mut liquidity_net = tick_info.liquidity_net;
                    if zero_for_one {
                        liquidity_net = -liquidity_net;
                    }

                    // Update liquidity
                    state.liquidity = if liquidity_net < 0 {
                        state.liquidity.saturating_sub((-liquidity_net) as u128)
                    } else {
                        state.liquidity.saturating_add(liquidity_net as u128)
                    };
                }

                // Update tick
                state.tick = if zero_for_one {
                    tick_next - 1
                } else {
                    tick_next
                };
            } else if state.sqrt_price_x64 != self.sqrt_price_x64 {
                // Price changed but didn't cross tick
                state.tick = get_tick_at_sqrt_price(state.sqrt_price_x64).unwrap_or(state.tick);
            }
        }

        // Calculate final amounts
        let (amount_in, amount_out) = if is_base_input {
            (
                amount_specified.saturating_sub(state.amount_specified_remaining),
                state.amount_calculated,
            )
        } else {
            (
                state.amount_calculated,
                amount_specified.saturating_sub(state.amount_specified_remaining),
            )
        };

        // Create new state
        let new_state = ClmmPoolState {
            sqrt_price_x64: state.sqrt_price_x64,
            tick: state.tick,
            tick_spacing: self.tick_spacing,
            liquidity: state.liquidity,
            fee_rate: self.fee_rate,
            ticks: self.ticks.clone(),
        };

        (amount_in, amount_out, new_state)
    }

    /// Get the current price (not sqrt price).
    pub fn price(&self) -> f64 {
        let sqrt = (self.sqrt_price_x64 as f64) / (Q64 as f64);
        sqrt * sqrt
    }
}

/// Internal swap state during iteration.
#[derive(Debug, Clone)]
struct SwapState {
    amount_specified_remaining: u64,
    amount_calculated: u64,
    sqrt_price_x64: u128,
    tick: i32,
    liquidity: u128,
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqrt_price_at_tick() {
        // tick = 0 should give sqrt_price = 1.0 in Q64.64
        let sqrt_at_0 = get_sqrt_price_at_tick(0).unwrap();
        // Q64 = 2^64, so sqrt_price should be close to 2^64
        assert!(sqrt_at_0 > Q64 / 2 && sqrt_at_0 < Q64 * 2);

        // Price increases with tick
        let sqrt_at_100 = get_sqrt_price_at_tick(100).unwrap();
        let sqrt_at_neg100 = get_sqrt_price_at_tick(-100).unwrap();
        assert!(sqrt_at_100 > sqrt_at_0);
        assert!(sqrt_at_neg100 < sqrt_at_0);
    }

    #[test]
    fn test_clmm_swap_basic() {
        // Create a pool: sqrt_price at tick 0, with liquidity
        let sqrt_price = get_sqrt_price_at_tick(0).unwrap();
        let mut pool = ClmmPoolState::new(
            sqrt_price,
            0,                 // current tick
            10,                // tick spacing
            1_000_000_000_000, // liquidity (1T)
            3000,              // 0.3% fee
        );

        // Add some ticks
        pool.add_tick(-1000, 100_000_000_000);
        pool.add_tick(1000, -100_000_000_000);

        // Simulate a swap: 1 SOL for token B (zero_for_one = true)
        let (amount_in, amount_out, new_state) = pool.simulate_swap(
            1_000_000_000, // 1 SOL in lamports
            true,          // zero_for_one
            true,          // exact input
            0,             // no price limit
        );

        assert!(amount_in > 0, "Should consume input");
        assert!(amount_out > 0, "Should produce output");
        assert!(
            new_state.sqrt_price_x64 < pool.sqrt_price_x64,
            "Price should decrease"
        );
    }
}
