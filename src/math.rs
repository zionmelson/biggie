//! Unified swap math for all DEX types
//!
//! Simple, inline calculations. No trait abstractions.
//! CLMM pools use sqrt_price for accurate price, with simplified swap math for small amounts.

use crate::pools::{DexType, Pool};

/// Q64 fixed point multiplier (2^64)
const Q64: u128 = 1u128 << 64;

/// Calculate swap output for a given input amount
pub fn calculate_swap(pool: &Pool, amount_in: u64, a_to_b: bool) -> Option<u64> {
    match pool.dex {
        DexType::RaydiumAmm | DexType::MeteoraDamm => {
            if pool.reserve_a == 0 || pool.reserve_b == 0 {
                return None;
            }
            calculate_amm_swap(pool, amount_in, a_to_b)
        }
        DexType::RaydiumClmm | DexType::OrcaWhirlpool | DexType::PancakeSwapClmm => {
            calculate_clmm_swap(pool, amount_in, a_to_b)
        }
        DexType::MeteoraDlmm => {
            // DLMM swap using bin price
            calculate_dlmm_swap(pool, amount_in, a_to_b)
        }
    }
}

/// CLMM swap calculation using sqrt_price and liquidity
/// 
/// For concentrated liquidity, within a single tick range:
/// - a_to_b (sell token A for B): Δb = L * (√P_current - √P_new)
/// - b_to_a (sell token B for A): Δa = L * (1/√P_new - 1/√P_current)
/// 
/// This is accurate for swaps that don't cross tick boundaries.
fn calculate_clmm_swap(pool: &Pool, amount_in: u64, a_to_b: bool) -> Option<u64> {
    let sqrt_price = pool.sqrt_price_x64?;
    let liquidity = pool.liquidity?;
    
    if liquidity == 0 || sqrt_price == 0 {
        return None;
    }
    
    let amount_in = amount_in as u128;
    
    // Apply fee to input
    let fee_bps = pool.fee_bps as u128;
    let amount_in_after_fee = amount_in * (10000 - fee_bps) / 10000;
    
    if a_to_b {
        // Selling token A for token B (e.g., SOL → USD)
        // 
        // Formula derivation:
        // new_sqrt_price = L * sqrt_price / (L + dx * sqrt_price / Q64)
        // delta_sqrt_price = sqrt_price - new_sqrt_price
        //                  = sqrt_price² * dx / (Q64 * L + dx * sqrt_price)
        // amount_out = L * delta_sqrt_price / Q64
        //
        // To avoid overflow with u128, we compute in steps:
        // delta_sqrt = (sqrt_price * sqrt_price / Q64) * dx / (L + dx * sqrt_price / Q64)
        // amount_out = L * delta_sqrt / Q64
        
        // sqrt_price² / Q64 (this is sqrt_price in Q64 representation of itself)
        let sqrt_price_sq_div_q64 = (sqrt_price as u128)
            .checked_mul(sqrt_price as u128)?
            .checked_div(Q64)?;
        
        // denominator = L + dx * sqrt_price / Q64
        let dx_times_sqrt = amount_in_after_fee.checked_mul(sqrt_price as u128)?.checked_div(Q64)?;
        let denominator = liquidity.checked_add(dx_times_sqrt)?;
        
        if denominator == 0 {
            return None;
        }
        
        // delta_sqrt_price = sqrt_price² / Q64 * dx / denominator
        let delta_sqrt_price = sqrt_price_sq_div_q64
            .checked_mul(amount_in_after_fee)?
            .checked_div(denominator)?;
        
        // amount_out = L * delta_sqrt_price / Q64
        let amount_out = liquidity
            .checked_mul(delta_sqrt_price)?
            .checked_div(Q64)?;
        
        Some(amount_out as u64)
    } else {
        // Selling token B for token A
        // New sqrt_price: √P_new = √P + Δb * Q64 / L
        // Output: Δa = L * Q64 * (1/√P - 1/√P_new)
        //       = L * Q64 * (√P_new - √P) / (√P * √P_new)
        
        // Calculate new sqrt_price
        let delta_sqrt_price = (amount_in_after_fee * Q64) / liquidity;
        let new_sqrt_price = sqrt_price + delta_sqrt_price;
        
        if new_sqrt_price == 0 {
            return None;
        }
        
        // amount_out = L * Q64 * delta_sqrt_price / (sqrt_price * new_sqrt_price)
        // Rewrite to avoid overflow: (L * delta_sqrt_price / sqrt_price) * Q64 / new_sqrt_price
        let temp = (liquidity * delta_sqrt_price) / sqrt_price;
        let amount_out = (temp * Q64) / new_sqrt_price;
        
        Some(amount_out as u64)
    }
}

/// DLMM swap calculation using active bin price
/// 
/// DLMM uses discrete bins with fixed price. Within a bin:
/// - Swap at the bin's price until liquidity depletes
/// - For simplicity, assume sufficient liquidity in active bin
/// 
/// Price per bin = (1 + bin_step/10000)^active_bin_id
/// 
/// IMPORTANT: The raw price from the formula is in smallest-unit-to-smallest-unit terms.
/// e.g., if price_raw = 7.2 for USD1/SOL pool, it means 1 micro-USD1 = 7.2 lamports.
/// Do NOT apply decimal adjustment for swap calculations - amounts are already in smallest units.
fn calculate_dlmm_swap(pool: &Pool, amount_in: u64, a_to_b: bool) -> Option<u64> {
    let bin_step = pool.bin_step? as f64;
    let active_bin_id = pool.active_bin_id?;
    
    // Calculate bin price: (1 + bin_step / 10000)^active_bin_id
    // This gives price in smallest-unit-to-smallest-unit terms
    // 1 unit of token_a (smallest) = price units of token_b (smallest)
    let base = 1.0 + bin_step / 10000.0;
    let price = base.powi(active_bin_id);
    
    // Apply fee
    let fee_multiplier = 1.0 - (pool.fee_bps as f64 / 10000.0);
    let amount_in_f = amount_in as f64 * fee_multiplier;
    
    let amount_out = if a_to_b {
        // Selling token A for token B
        // 1 unit of A = price units of B
        amount_in_f * price
    } else {
        // Selling token B for token A  
        // 1 unit of B = 1/price units of A
        amount_in_f / price
    };
    
    // Sanity check
    if amount_out <= 0.0 || amount_out > u64::MAX as f64 {
        return None;
    }
    
    Some(amount_out as u64)
}

/// Constant product AMM swap: x * y = k
/// output = (y * dx) / (x + dx) - fee
fn calculate_amm_swap(pool: &Pool, amount_in: u64, a_to_b: bool) -> Option<u64> {
    let (reserve_in, reserve_out) = if a_to_b {
        (pool.reserve_a as u128, pool.reserve_b as u128)
    } else {
        (pool.reserve_b as u128, pool.reserve_a as u128)
    };

    let amount_in = amount_in as u128;
    
    // Apply fee to input
    let fee_bps = pool.fee_bps as u128;
    let amount_in_after_fee = amount_in * (10000 - fee_bps) / 10000;

    // Constant product formula
    let numerator = reserve_out * amount_in_after_fee;
    let denominator = reserve_in + amount_in_after_fee;
    
    if denominator == 0 {
        return None;
    }

    let amount_out = numerator / denominator;
    
    // Sanity check: output shouldn't exceed reserves
    if amount_out >= reserve_out {
        return None;
    }

    Some(amount_out as u64)
}

/// Calculate effective price: how much token B per 1 token A
/// 
/// For AMM: price = reserve_b / reserve_a (adjusted for decimals)
/// For CLMM: price = (sqrt_price / 2^64)^2 (adjusted for decimals)
/// For DLMM: price = (1 + bin_step/10000)^active_bin_id (adjusted for decimals)
pub fn calculate_price(pool: &Pool, base_is_a: bool) -> Option<f64> {
    match pool.dex {
        DexType::RaydiumClmm | DexType::OrcaWhirlpool | DexType::PancakeSwapClmm => {
            calculate_clmm_price(pool, base_is_a)
        }
        DexType::MeteoraDlmm => {
            calculate_dlmm_price(pool, base_is_a)
        }
        _ => calculate_amm_price(pool, base_is_a)
    }
}

/// CLMM price from sqrt_price_x64
/// 
/// sqrt_price_x64 = sqrt(price) * 2^64
/// price = (sqrt_price_x64 / 2^64)^2
/// 
/// This is the price of token A in terms of token B.
pub fn calculate_clmm_price(pool: &Pool, base_is_a: bool) -> Option<f64> {
    let sqrt_price = pool.sqrt_price_x64? as f64;
    let q64 = (1u128 << 64) as f64;
    
    // price_a_per_b = (sqrt_price / 2^64)^2
    let price_raw = (sqrt_price / q64).powi(2);
    
    // Adjust for decimals: if decimals_a=9, decimals_b=6, multiply by 10^(9-6)=1000
    let decimal_adjustment = 10f64.powi(pool.decimals_a as i32 - pool.decimals_b as i32);
    let price_a_in_b = price_raw * decimal_adjustment;
    
    if base_is_a {
        Some(price_a_in_b)
    } else {
        Some(1.0 / price_a_in_b)
    }
}

/// DLMM price from active bin
/// 
/// DLMM uses discrete price bins where:
/// price = (1 + bin_step/10000)^active_bin_id
/// 
/// bin_step is in basis points (e.g., 10 = 0.1% between bins)
pub fn calculate_dlmm_price(pool: &Pool, base_is_a: bool) -> Option<f64> {
    let bin_step = pool.bin_step? as f64;
    let active_bin_id = pool.active_bin_id?;
    
    // Base price factor: (1 + bin_step / 10000)
    let base = 1.0 + bin_step / 10000.0;
    
    // Price = base^active_bin_id
    let price_raw = base.powi(active_bin_id);
    
    // Adjust for decimals
    let decimal_adjustment = 10f64.powi(pool.decimals_a as i32 - pool.decimals_b as i32);
    let price_a_in_b = price_raw * decimal_adjustment;
    
    if base_is_a {
        Some(price_a_in_b)
    } else {
        Some(1.0 / price_a_in_b)
    }
}

/// AMM price from reserves
fn calculate_amm_price(pool: &Pool, base_is_a: bool) -> Option<f64> {
    if pool.reserve_a == 0 || pool.reserve_b == 0 {
        return None;
    }

    let (base_reserve, quote_reserve, base_decimals, quote_decimals) = if base_is_a {
        (pool.reserve_a, pool.reserve_b, pool.decimals_a, pool.decimals_b)
    } else {
        (pool.reserve_b, pool.reserve_a, pool.decimals_b, pool.decimals_a)
    };

    // Adjust for decimals
    let base_adjusted = base_reserve as f64 / 10f64.powi(base_decimals as i32);
    let quote_adjusted = quote_reserve as f64 / 10f64.powi(quote_decimals as i32);

    Some(quote_adjusted / base_adjusted)
}

/// Calculate price from tick index
/// price = 1.0001^tick
pub fn price_from_tick(tick: i32, decimals_a: u8, decimals_b: u8) -> f64 {
    let price_raw = 1.0001f64.powi(tick);
    let decimal_adjustment = 10f64.powi(decimals_a as i32 - decimals_b as i32);
    price_raw * decimal_adjustment
}

/// Calculate how much input needed to get desired output (reverse calculation)
pub fn calculate_swap_input(pool: &Pool, amount_out: u64, a_to_b: bool) -> Option<u64> {
    let (reserve_in, reserve_out) = if a_to_b {
        (pool.reserve_a as u128, pool.reserve_b as u128)
    } else {
        (pool.reserve_b as u128, pool.reserve_a as u128)
    };

    let amount_out = amount_out as u128;
    
    if amount_out >= reserve_out {
        return None;
    }

    // Reverse constant product: dx = (x * dy) / (y - dy)
    let numerator = reserve_in * amount_out;
    let denominator = reserve_out - amount_out;
    
    if denominator == 0 {
        return None;
    }

    let amount_in_before_fee = numerator / denominator + 1; // Round up
    
    // Add fee back
    let fee_bps = pool.fee_bps as u128;
    let amount_in = amount_in_before_fee * 10000 / (10000 - fee_bps) + 1;

    Some(amount_in as u64)
}

/// Apply slippage to output amount
pub fn apply_slippage(amount: u64, slippage_bps: u16) -> u64 {
    let slippage = slippage_bps as u64;
    amount * (10000 - slippage) / 10000
}

/// Calculate price impact in basis points
pub fn calculate_price_impact(pool: &Pool, amount_in: u64, a_to_b: bool) -> Option<u16> {
    let small_amount = 1000u64; // Tiny amount for spot price
    let spot_output = calculate_swap(pool, small_amount, a_to_b)? as u128;
    let actual_output = calculate_swap(pool, amount_in, a_to_b)? as u128;
    
    // Expected output at spot price
    let expected_output = spot_output * (amount_in as u128) / (small_amount as u128);
    
    if expected_output == 0 {
        return None;
    }

    // Impact = (expected - actual) / expected * 10000
    let impact = if actual_output < expected_output {
        ((expected_output - actual_output) * 10000 / expected_output) as u16
    } else {
        0
    };

    Some(impact)
}

/// Flash loan fee for Solend (30 bps = 0.3%)
pub const FLASH_LOAN_FEE_BPS: u64 = 30;

/// Calculate flash loan fee
pub fn flash_loan_fee(amount: u64) -> u64 {
    amount * FLASH_LOAN_FEE_BPS / 10000
}

/// Minimum profitable spread accounting for:
/// - Flash loan fee (30 bps)
/// - Two swap fees (varies, ~25 bps each)
/// - Slippage buffer
/// - Jito tip
pub fn minimum_spread_bps(pool_a_fee: u16, pool_b_fee: u16, slippage_bps: u16) -> u16 {
    FLASH_LOAN_FEE_BPS as u16 + pool_a_fee + pool_b_fee + slippage_bps
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::pubkey::Pubkey;

    fn make_test_pool(reserve_a: u64, reserve_b: u64, fee_bps: u16) -> Pool {
        Pool {
            address: Pubkey::new_unique(),
            dex: DexType::RaydiumAmm,
            mint_a: Pubkey::new_unique(),
            mint_b: Pubkey::new_unique(),
            vault_a: Pubkey::new_unique(),
            vault_b: Pubkey::new_unique(),
            decimals_a: 9,
            decimals_b: 6,
            reserve_a,
            reserve_b,
            fee_bps,
            tick_spacing: None,
            current_tick: None,
            bin_step: None,
            active_bin_id: None,
        }
    }

    #[test]
    fn test_amm_swap() {
        // Pool with 1000 SOL and 100,000 USDC (price ~$100/SOL)
        let pool = make_test_pool(
            1_000_000_000_000,  // 1000 SOL
            100_000_000_000,    // 100,000 USDC
            25, // 0.25% fee
        );

        // Swap 1 SOL for USDC
        let output = calculate_swap(&pool, 1_000_000_000, true).unwrap();
        
        // Should get ~99.7 USDC (minus fee and slippage)
        assert!(output > 99_000_000 && output < 100_000_000);
    }

    #[test]
    fn test_price_calculation() {
        let pool = make_test_pool(
            1_000_000_000_000,  // 1000 SOL (9 decimals)
            100_000_000_000,    // 100,000 USDC (6 decimals)
            25,
        );

        // Price of SOL in USDC terms (base = SOL = A)
        let price = calculate_price(&pool, true).unwrap();
        assert!((price - 100.0).abs() < 0.01); // Should be ~100
    }

    #[test]
    fn test_slippage() {
        let amount = 1_000_000_000u64;
        let with_slippage = apply_slippage(amount, 150); // 1.5%
        assert_eq!(with_slippage, 985_000_000);
    }
}
