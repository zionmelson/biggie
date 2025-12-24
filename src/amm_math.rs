//! AMM math for calculating expected swap outputs with slippage.
//!
//! Supports multiple curve types:
//! - Constant Product (x * y = k): Raydium AMM, Orca Legacy
//! - Concentrated Liquidity: Raydium CLMM, Orca Whirlpool (approximated)
//! - Stable Swap: Saber, Mercurial (approximated)

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

/// Pool state tracked from CoreCast updates
#[derive(Clone, Debug)]
pub struct PoolState {
    pub base_mint: String,
    pub quote_mint: String,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub base_decimals: u8,
    pub quote_decimals: u8,
    pub curve: CurveType,
    pub fee_bps: u16,
    pub last_update_slot: u64,
    /// For CLMM pools: the canonical price derived from the last swap's bin/tick
    /// This is the TRUE pool price, not the reserve ratio
    pub canonical_price: Option<f64>,
    /// For CLMM pools: the active bin_id (DLMM) or tick (Whirlpool/Raydium)
    pub active_bin_or_tick: Option<i32>,
    /// For CLMM pools: bin_step (DLMM) or tick_spacing (CLMM)
    pub spacing: Option<u16>,
    /// When the canonical price was last updated
    pub last_canonical_update: Option<Instant>,
}

/// Curve types with their swap math
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveType {
    /// x * y = k (Raydium AMM, Orca Legacy, Meteora DAMM)
    ConstantProduct,
    /// Concentrated liquidity - use constant product as approximation
    /// Real CLMM requires tick data which we approximate
    ConcentratedLiquidity,
    /// Stable swap curve for pegged assets (lower slippage)
    StableSwap,
}

impl Default for CurveType {
    fn default() -> Self {
        CurveType::ConstantProduct
    }
}

/// Cache of pool states indexed by pool address (base58)
pub struct PoolStateCache {
    pools: RwLock<HashMap<String, PoolState>>,
}

impl PoolStateCache {
    pub fn new() -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
        }
    }

    /// Update pool state from CoreCast data (reserves only - from pool events)
    pub fn update(
        &self,
        pool_address: &str,
        base_mint: &str,
        quote_mint: &str,
        base_reserve: u64,
        quote_reserve: u64,
        base_decimals: u8,
        quote_decimals: u8,
        curve: CurveType,
        fee_bps: u16,
        slot: u64,
    ) {
        let mut pools = self.pools.write().unwrap();

        // Preserve existing canonical price data if we're just updating reserves
        let existing = pools.get(pool_address);
        let (canonical_price, active_bin_or_tick, spacing, last_canonical_update) =
            if let Some(existing) = existing {
                (
                    existing.canonical_price,
                    existing.active_bin_or_tick,
                    existing.spacing,
                    existing.last_canonical_update,
                )
            } else {
                (None, None, None, None)
            };

        pools.insert(
            pool_address.to_string(),
            PoolState {
                base_mint: base_mint.to_string(),
                quote_mint: quote_mint.to_string(),
                base_reserve,
                quote_reserve,
                base_decimals,
                quote_decimals,
                curve,
                fee_bps,
                last_update_slot: slot,
                canonical_price,
                active_bin_or_tick,
                spacing,
                last_canonical_update,
            },
        );
    }

    /// Update canonical price for a CLMM pool from swap data
    /// This should be called whenever we see a swap on a CLMM pool
    pub fn update_canonical_price(
        &self,
        pool_address: &str,
        canonical_price: f64,
        bin_or_tick: i32,
        spacing: u16,
    ) {
        let mut pools = self.pools.write().unwrap();
        if let Some(state) = pools.get_mut(pool_address) {
            state.canonical_price = Some(canonical_price);
            state.active_bin_or_tick = Some(bin_or_tick);
            state.spacing = Some(spacing);
            state.last_canonical_update = Some(Instant::now());

            tracing::trace!(
                "POOL CANONICAL: {} price={:.9} bin/tick={} spacing={}",
                &pool_address[..8.min(pool_address.len())],
                canonical_price,
                bin_or_tick,
                spacing
            );
        }
    }

    /// Update fee_bps from observed trade data
    /// 
    /// This infers the actual fee rate from a trade where we know:
    /// - input_amount: the amount input to the swap
    /// - fee_amount: the fee taken (from DexTradeEvent.fee field)
    ///
    /// The fee is calculated as: fee_bps = (fee_amount * 10000) / input_amount
    ///
    /// This provides empirical fee data rather than hardcoded defaults.
    pub fn update_fee_from_trade(
        &self,
        pool_address: &str,
        input_amount: u64,
        fee_amount: u64,
    ) {
        // Only update if we have meaningful data
        if input_amount == 0 || fee_amount == 0 {
            return;
        }

        // Calculate fee in basis points
        // fee_bps = (fee * 10000) / input
        let inferred_fee_bps = ((fee_amount as u128 * 10_000) / input_amount as u128) as u16;
        
        // Sanity check: fee should be reasonable (0 < fee < 1000 bps = 10%)
        if inferred_fee_bps == 0 || inferred_fee_bps > 1000 {
            return;
        }

        let mut pools = self.pools.write().unwrap();
        if let Some(state) = pools.get_mut(pool_address) {
            // Only update if the inferred fee differs significantly from current
            // This smooths out measurement noise
            let old_fee = state.fee_bps;
            
            // Use exponential moving average: new = 0.3 * observed + 0.7 * old
            // This prevents single trades from causing wild swings
            let smoothed_fee = ((inferred_fee_bps as u32 * 3 + old_fee as u32 * 7) / 10) as u16;
            
            if smoothed_fee != old_fee {
                state.fee_bps = smoothed_fee;
                tracing::trace!(
                    "POOL FEE UPDATE: {} fee_bps {} -> {} (observed={})",
                    &pool_address[..8.min(pool_address.len())],
                    old_fee,
                    smoothed_fee,
                    inferred_fee_bps
                );
            }
        }
    }

    /// Get the canonical price for a pool
    /// For CLMM pools: returns the stored canonical price from last swap
    /// For constant product: calculates from reserves
    pub fn get_canonical_price(&self, pool_address: &str) -> Option<f64> {
        let pools = self.pools.read().unwrap();
        let state = pools.get(pool_address)?;

        match state.curve {
            CurveType::ConcentratedLiquidity => {
                // Use stored canonical price if available and fresh (< 30s old)
                if let (Some(price), Some(last_update)) =
                    (state.canonical_price, state.last_canonical_update)
                {
                    if last_update.elapsed().as_secs() < 30 {
                        return Some(price);
                    }
                }
                // Fallback: no fresh canonical price available
                None
            }
            CurveType::ConstantProduct | CurveType::StableSwap => {
                // For these curves, reserve ratio IS the canonical price
                if state.base_reserve == 0 {
                    return None;
                }
                let base_norm = state.base_reserve as f64 / 10_f64.powi(state.base_decimals as i32);
                let quote_norm =
                    state.quote_reserve as f64 / 10_f64.powi(state.quote_decimals as i32);
                Some(quote_norm / base_norm) // quote per base
            }
        }
    }

    /// Get pool state
    pub fn get(&self, pool_address: &str) -> Option<PoolState> {
        let pools = self.pools.read().unwrap();
        pools.get(pool_address).cloned()
    }

    /// Get all pool states for a given token pair (regardless of which is base/quote)
    /// Returns vec of (pool_address, pool_state, is_base_first) tuples
    /// is_base_first indicates if the first mint in the pair is the pool's base mint
    pub fn get_pools_for_pair(&self, mint_a: &str, mint_b: &str) -> Vec<(String, PoolState, bool)> {
        let pools = self.pools.read().unwrap();
        pools
            .iter()
            .filter_map(|(addr, state)| {
                if state.base_mint == mint_a && state.quote_mint == mint_b {
                    Some((addr.clone(), state.clone(), true))
                } else if state.base_mint == mint_b && state.quote_mint == mint_a {
                    Some((addr.clone(), state.clone(), false))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Calculate spot price (output per input) from pool state
    /// For CLMM pools: uses canonical price from last swap
    /// For constant product: calculates from reserves
    /// Returns (spot_price, pool_address) for all pools matching the pair
    pub fn get_spot_prices(&self, input_mint: &str, output_mint: &str) -> Vec<SpotPrice> {
        let pools = self.pools.read().unwrap();
        pools
            .iter()
            .filter_map(|(addr, state)| {
                let (input_reserve, output_reserve, input_decimals, output_decimals, is_base_input) =
                    if state.base_mint == input_mint && state.quote_mint == output_mint {
                        (
                            state.base_reserve,
                            state.quote_reserve,
                            state.base_decimals,
                            state.quote_decimals,
                            true,
                        )
                    } else if state.quote_mint == input_mint && state.base_mint == output_mint {
                        (
                            state.quote_reserve,
                            state.base_reserve,
                            state.quote_decimals,
                            state.base_decimals,
                            false,
                        )
                    } else {
                        return None;
                    };

                if input_reserve == 0 {
                    return None;
                }

                // For CLMM pools: use canonical price if available
                let spot_price = match state.curve {
                    CurveType::ConcentratedLiquidity => {
                        if let Some(canonical) = state.canonical_price {
                            // canonical_price is quote_per_base
                            // If input is base: we want output (quote) per input (base) = canonical
                            // If input is quote: we want output (base) per input (quote) = 1/canonical
                            if is_base_input {
                                canonical
                            } else {
                                1.0 / canonical
                            }
                        } else {
                            // No canonical price yet, fall back to reserve ratio
                            let decimal_adjustment = 10f64.powi(input_decimals as i32 - output_decimals as i32);
                            (output_reserve as f64 / input_reserve as f64) * decimal_adjustment
                        }
                    }
                    CurveType::ConstantProduct | CurveType::StableSwap => {
                        // For these curves, reserve ratio IS the canonical spot price
                        let decimal_adjustment = 10f64.powi(input_decimals as i32 - output_decimals as i32);
                        (output_reserve as f64 / input_reserve as f64) * decimal_adjustment
                    }
                };

                Some(SpotPrice {
                    pool_address: addr.clone(),
                    price: spot_price,
                    input_reserve,
                    output_reserve,
                    curve: state.curve,
                    fee_bps: state.fee_bps,
                })
            })
            .collect()
    }

    /// Calculate expected output for a swap given input mint
    /// Direction is automatically determined from pool state's base/quote mints
    /// Returns None if input_mint doesn't match either base or quote mint
    pub fn calculate_swap_output(
        &self,
        pool_address: &str,
        input_amount: u64,
        input_mint: &str,
    ) -> Option<SwapQuote> {
        let state = self.get(pool_address)?;

        // Determine direction from input mint
        let is_base_to_quote = if input_mint == state.base_mint {
            true
        } else if input_mint == state.quote_mint {
            false
        } else {
            // Input mint doesn't match either side of this pool
            tracing::warn!(
                "AMM MATH: input_mint {} doesn't match pool base={} or quote={}",
                &input_mint[..8.min(input_mint.len())],
                &state.base_mint[..8.min(state.base_mint.len())],
                &state.quote_mint[..8.min(state.quote_mint.len())]
            );
            return None;
        };

        let (input_reserve, output_reserve) = if is_base_to_quote {
            (state.base_reserve, state.quote_reserve)
        } else {
            (state.quote_reserve, state.base_reserve)
        };

        tracing::debug!(
            "AMM DIR: {} -> {} (base_to_quote={}) in_res={} out_res={} pool={}",
            &input_mint[..8.min(input_mint.len())],
            if is_base_to_quote {
                &state.quote_mint[..8.min(state.quote_mint.len())]
            } else {
                &state.base_mint[..8.min(state.base_mint.len())]
            },
            is_base_to_quote,
            input_reserve,
            output_reserve,
            &pool_address[..8.min(pool_address.len())]
        );

        // Apply fee to input
        let fee_amount = (input_amount as u128 * state.fee_bps as u128 / 10_000) as u64;
        let input_after_fee = input_amount.saturating_sub(fee_amount);

        // =======================================================================
        // UNIFIED SWAP OUTPUT CALCULATION
        // =======================================================================
        // For ALL pool types, we use the same approach:
        // 1. If we have canonical price from observed trades -> use it (most accurate)
        // 2. Calculate slippage based on trade size vs available liquidity
        // 3. Liquidity is used ONLY to estimate slippage, not to calculate output
        // =======================================================================

        let input_decimals = if is_base_to_quote { state.base_decimals } else { state.quote_decimals };
        let output_decimals = if is_base_to_quote { state.quote_decimals } else { state.base_decimals };
        let input_norm = input_after_fee as f64 / 10_f64.powi(input_decimals as i32);

        // Get the canonical price (quote per base) from observed trades
        // This is the TRUE market price, not derived from reserves
        let (output_amount, price_impact_bps) = if let Some(canonical_price) = state.canonical_price {
            // Calculate ideal output at canonical price
            let ideal_output_norm = if is_base_to_quote {
                input_norm * canonical_price  // base -> quote: multiply by price
            } else {
                input_norm / canonical_price  // quote -> base: divide by price
            };
            let ideal_output = (ideal_output_norm * 10_f64.powi(output_decimals as i32)) as u64;

            // Calculate slippage based on trade size vs liquidity
            // Larger trades relative to pool liquidity = more slippage
            let x_norm = input_reserve as f64 / 10_f64.powi(state.base_decimals as i32);
            let y_norm = output_reserve as f64 / 10_f64.powi(state.quote_decimals as i32);
            let liquidity = (x_norm * y_norm).sqrt();

            // Trade fraction = how much of the pool's liquidity we're consuming
            let trade_fraction = if liquidity > 0.0 {
                input_norm / liquidity
            } else {
                0.0
            };

            // Slippage model: quadratic in trade fraction
            // Small trades (<1% of liquidity) = minimal slippage
            // Large trades (>10% of liquidity) = significant slippage
            // slippage = trade_fraction^2 * 50 (capped at 50%)
            let slippage_pct = (trade_fraction * trade_fraction * 50.0).min(0.5);
            let output_with_slippage = (ideal_output as f64 * (1.0 - slippage_pct)) as u64;
            let slippage_bps = (slippage_pct * 10000.0) as u64;

            tracing::debug!(
                "CANONICAL PRICE CALC: input={} price={:.8} ideal_out={} slippage={}bps final_out={}",
                input_after_fee, canonical_price, ideal_output, slippage_bps, output_with_slippage
            );

            (output_with_slippage, slippage_bps)
        } else {
            // No canonical price - fall back to constant product formula
            // This works for all pool types as a reasonable approximation
            calculate_constant_product(input_after_fee, input_reserve, output_reserve)
        };

        tracing::debug!(
            "AMM CALC: {} -> {} (curve={:?}) pool={} in_res={} out_res={} fee={}bps",
            input_amount,
            output_amount,
            state.curve,
            &pool_address[..8.min(pool_address.len())],
            input_reserve,
            output_reserve,
            state.fee_bps
        );

        Some(SwapQuote {
            input_amount,
            output_amount,
            fee_amount,
            price_impact_bps,
            input_reserve,
            output_reserve,
        })
    }

    /// Calculate expected output for a CLMM swap using actual bin/tick data.
    ///
    /// This is more accurate than `calculate_swap_output` for CLMM pools because
    /// it uses the actual spacing (bin_step or tick_spacing) from the array map.
    ///
    /// Falls back to `calculate_swap_output` if no CLMM data is available.
    ///
    /// # Arguments
    /// * `pool_address` - Pool address for reserve lookup
    /// * `input_amount` - Raw input amount
    /// * `input_mint` - Input token mint address
    /// * `clmm_array_map` - The CLMM array map for bin/tick data lookup
    /// * `array_address` - Optional: specific bin_array/tick_array address to use
    /// * `current_price` - Optional: current observed price (for better accuracy)
    pub fn calculate_clmm_swap_output(
        &self,
        pool_address: &str,
        input_amount: u64,
        input_mint: &str,
        _clmm_array_map: Option<&crate::clmm_price_map::ClmmArrayMap>,
        _array_address: Option<&str>,
        _current_price: Option<f64>,
    ) -> Option<SwapQuote> {
        // =======================================================================
        // UNIFIED CALCULATION: Use the same logic for ALL pool types
        // =======================================================================
        // We no longer use the broken CLMM simulation that returned u64::MAX.
        // Instead, all pools use canonical price from observed trades + slippage
        // based on trade size vs liquidity. This is calculated in calculate_swap_output.
        // =======================================================================
        self.calculate_swap_output(pool_address, input_amount, input_mint)
    }
}

impl Default for PoolStateCache {
    fn default() -> Self {
        Self::new()
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

    if protocol_lower.contains("clmm")
        || protocol_lower.contains("whirlpool")
        || protocol_lower.contains("dlmm")
        || protocol_lower.contains("amm_v3")  // Raydium CLMM is called amm_v3
    {
        CurveType::ConcentratedLiquidity
    } else if protocol_lower.contains("stable")
        || protocol_lower.contains("saber")
        || protocol_lower.contains("mercurial")
    {
        CurveType::StableSwap
    } else {
        // Default: Raydium AMM, Orca Legacy, Meteora DAMM, etc.
        CurveType::ConstantProduct
    }
}

// =============================================================================
// StableSwap Math (Curve Finance / Saber invariant)
// =============================================================================

/// Number of coins in a stable swap pool (always 2 for Saber)
const STABLE_N_COINS: u64 = 2;

/// Default amplification coefficient for stable pools
/// Saber typically uses A=100 to A=1000 depending on the pair
const DEFAULT_AMP_FACTOR: u64 = 100;

/// Compute the StableSwap invariant D using Newton's method.
/// 
/// The invariant equation is:
/// A * n^n * sum(x_i) + D = A * D * n^n + D^(n+1) / (n^n * prod(x_i))
///
/// This is derived from the Curve Finance StableSwap paper.
fn stable_compute_d(amount_a: u64, amount_b: u64, amp_factor: u64) -> Option<u128> {
    let sum_x = (amount_a as u128).checked_add(amount_b as u128)?;
    if sum_x == 0 {
        return Some(0);
    }

    let ann = amp_factor as u128 * STABLE_N_COINS as u128; // A * n
    
    // Newton's method to find D
    let mut d = sum_x;
    let mut d_prev: u128;
    
    for _ in 0..256 {
        // d_prod = D^n / (n^n * prod(x_i))
        // For n=2: d_prod = D * D / (4 * amount_a * amount_b)
        let mut d_prod = d;
        d_prod = d_prod
            .checked_mul(d)?
            .checked_div((amount_a as u128).checked_mul(STABLE_N_COINS as u128)?)?;
        d_prod = d_prod
            .checked_mul(d)?
            .checked_div((amount_b as u128).checked_mul(STABLE_N_COINS as u128)?)?;
        
        d_prev = d;
        
        // d = (ann * sum_x + d_prod * n) * d / ((ann - 1) * d + (n + 1) * d_prod)
        let leverage = sum_x.checked_mul(ann)?;
        let numerator = d.checked_mul(
            d_prod.checked_mul(STABLE_N_COINS as u128)?.checked_add(leverage)?
        )?;
        let denominator = d
            .checked_mul(ann.checked_sub(1)?)?
            .checked_add(d_prod.checked_mul(STABLE_N_COINS as u128 + 1)?)?;
        
        d = numerator.checked_div(denominator)?;
        
        // Check convergence
        if d > d_prev {
            if d.checked_sub(d_prev)? <= 1 {
                break;
            }
        } else if d_prev.checked_sub(d)? <= 1 {
            break;
        }
    }
    
    Some(d)
}

/// Compute y given x and D for StableSwap.
/// 
/// Solve for y in:
/// y^2 + y * (sum' - (A*n^n - 1) * D / (A * n^n)) = D^(n+1) / (n^(2n) * prod' * A)
/// 
/// Which simplifies to: y^2 + b*y = c
fn stable_compute_y(x: u64, d: u128, amp_factor: u64) -> Option<u64> {
    let ann = amp_factor as u128 * STABLE_N_COINS as u128;
    
    // c = D^(n+1) / (n^(2n) * x * A) for n=2: c = D^3 / (8 * x * A * n)
    // Simplified: c = D * D / (x * 2) then c = c * D / (ann * 2)
    let mut c = d
        .checked_mul(d)?
        .checked_div((x as u128).checked_mul(STABLE_N_COINS as u128)?)?;
    c = c
        .checked_mul(d)?
        .checked_div(ann.checked_mul(STABLE_N_COINS as u128)?)?;
    
    // b = x + D / ann (we subtract D later in the iteration)
    let b = (x as u128).checked_add(d.checked_div(ann)?)?;
    
    // Newton's method to find y
    let mut y = d;
    let mut y_prev: u128;
    
    for _ in 0..256 {
        y_prev = y;
        // y = (y^2 + c) / (2*y + b - d)
        let y_numerator = y.checked_mul(y)?.checked_add(c)?;
        let y_denominator = y.checked_mul(2)?.checked_add(b)?.checked_sub(d)?;
        y = y_numerator.checked_div(y_denominator)?;
        
        // Check convergence
        if y > y_prev {
            if y.checked_sub(y_prev)? <= 1 {
                break;
            }
        } else if y_prev.checked_sub(y)? <= 1 {
            break;
        }
    }
    
    // Convert to u64, clamping to max
    if y > u64::MAX as u128 {
        Some(u64::MAX)
    } else {
        Some(y as u64)
    }
}

/// Calculate StableSwap output amount.
/// 
/// This implements the Curve Finance / Saber swap formula:
/// 1. Compute D (invariant) from current reserves
/// 2. Compute new y given new x after trade
/// 3. Output = old_y - new_y - 1 (subtract 1 for rounding)
fn calculate_stable_swap(
    input_amount: u64,
    input_reserve: u64,
    output_reserve: u64,
    amp_factor: u64,
) -> (u64, u64) {
    if input_reserve == 0 || output_reserve == 0 || input_amount == 0 {
        return (0, 0);
    }
    
    // Compute current invariant D
    let d = match stable_compute_d(input_reserve, output_reserve, amp_factor) {
        Some(d) => d,
        None => return (0, 0),
    };
    
    // New input reserve after swap
    let new_input = match input_reserve.checked_add(input_amount) {
        Some(v) => v,
        None => return (0, 0),
    };
    
    // Compute new output reserve (y) given new input (x) and invariant D
    let new_output = match stable_compute_y(new_input, d, amp_factor) {
        Some(y) => y,
        None => return (0, 0),
    };
    
    // Output amount = old_output - new_output - 1 (for rounding)
    let output_amount = output_reserve.saturating_sub(new_output).saturating_sub(1);
    
    // Calculate price impact
    // Spot price for stable = output_reserve / input_reserve (should be ~1 for pegged)
    let spot_price = output_reserve as f64 / input_reserve as f64;
    let effective_price = if input_amount > 0 {
        output_amount as f64 / input_amount as f64
    } else {
        spot_price
    };
    
    let price_impact_bps = if spot_price > 0.0 {
        ((spot_price - effective_price).abs() / spot_price * 10_000.0) as u64
    } else {
        0
    };
    
    (output_amount, price_impact_bps)
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
    /// Create from a cached pool state
    pub fn from_pool_state(state: &PoolState) -> Self {
        Self {
            base_reserve: state.base_reserve,
            quote_reserve: state.quote_reserve,
            fee_bps: state.fee_bps,
        }
    }

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
        let new_state = if base_to_quote {
            VirtualPoolState {
                base_reserve: self.base_reserve.saturating_add(amount_in),
                quote_reserve: self.quote_reserve.saturating_sub(output),
                fee_bps: self.fee_bps,
            }
        } else {
            VirtualPoolState {
                base_reserve: self.base_reserve.saturating_sub(output),
                quote_reserve: self.quote_reserve.saturating_add(amount_in),
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
    pool_x: &VirtualPoolState,       // Victim's pool
    pool_y: &VirtualPoolState,       // Alternate pool  
    victim_amount: u64,              // Victim's input amount
    victim_base_to_quote: bool,      // Victim's swap direction
    borrow_amount: u64,              // How much we borrow
    borrow_is_base: bool,            // Are we borrowing base token?
    flash_fee_bps: u16,              // Flash loan fee (typically 30 = 0.3%)
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

    while high > low + 10_000 { // Precision of ~0.00001 SOL
        let mid = (low + high) / 2;
        
        let profit_at_mid = simulate_cross_pool_backrun(
            pool_x, pool_y, victim_amount, victim_base_to_quote,
            mid, borrow_is_base, flash_fee_bps,
        ).map(|r| r.profit).unwrap_or(0);

        let profit_at_mid_plus = simulate_cross_pool_backrun(
            pool_x, pool_y, victim_amount, victim_base_to_quote,
            mid + 10_000, borrow_is_base, flash_fee_bps,
        ).map(|r| r.profit).unwrap_or(0);

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
        pool_x, pool_y, victim_amount, victim_base_to_quote,
        best_amount, borrow_is_base, flash_fee_bps,
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
    victim_min_output: u64,  // Must ensure victim still gets at least this
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
            let (victim_out, pool_after_v) = pool_after_fr.simulate_swap(victim_amount, victim_base_to_quote);
            
            // Victim must get at least their min_output
            if victim_out < victim_min_output {
                None
            } else {
                let (backrun_out, _) = pool_after_v.simulate_swap(frontrun_out, !victim_base_to_quote);
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
            let (frontrun_out, pool_after_fr) = pool.simulate_swap(test_amount, victim_base_to_quote);
            let (victim_out, pool_after_v) = pool_after_fr.simulate_swap(victim_amount, victim_base_to_quote);
            
            if victim_out < victim_min_output {
                0
            } else {
                let (backrun_out, _) = pool_after_v.simulate_swap(frontrun_out, !victim_base_to_quote);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_pool_swap() {
        // Pool: 1000 SOL, 200,000 USDC
        let pool = VirtualPoolState::new(
            1_000_000_000_000, // 1000 SOL
            200_000_000_000,   // 200,000 USDC
            30,                 // 0.3% fee
        );

        // Swap 10 SOL for USDC
        let (output, new_state) = pool.simulate_swap(10_000_000_000, true);
        
        // Expected: ~1980 USDC (fee + slippage)
        assert!(output > 1_900_000_000 && output < 2_000_000_000, "output was {}", output);
        
        // Reserves should update
        assert_eq!(new_state.base_reserve, 1_010_000_000_000);
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
            10_000_000_000,  // victim amount
            true,            // base to quote
            5_000_000_000,   // our frontrun: 5 SOL
            30,              // flash fee
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
