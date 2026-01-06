//! Unified swap math for all DEX types
//!
//! Simple, inline calculations. No trait abstractions.
//! For CLMM/DLMM, we use simplified models that work for small swaps.

use crate::pools::{DexType, Pool};

/// Calculate swap output for a given input amount
/// Returns (output_amount, price_impact_bps)
pub fn calculate_swap(pool: &Pool, amount_in: u64, a_to_b: bool) -> Option<u64> {
    if pool.reserve_a == 0 || pool.reserve_b == 0 {
        return None;
    }

    match pool.dex {
        DexType::RaydiumAmm | DexType::MeteoraDamm => {
            calculate_amm_swap(pool, amount_in, a_to_b)
        }
        DexType::RaydiumClmm | DexType::OrcaWhirlpool => {
            // Simplified CLMM: treat as AMM for now
            // For accurate calc, would need tick arrays
            calculate_amm_swap(pool, amount_in, a_to_b)
        }
        DexType::MeteoraDlmm => {
            // Simplified DLMM: treat as AMM for now
            // For accurate calc, would need bin arrays
            calculate_amm_swap(pool, amount_in, a_to_b)
        }
    }
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

/// Calculate effective price: how much quote per 1 base token
/// Higher = base token is more valuable
pub fn calculate_price(pool: &Pool, base_is_a: bool) -> Option<f64> {
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
