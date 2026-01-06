//! Dynamic Liquidity Market Maker (DLMM) Math
//!
//! Implements bin-based swap simulation for Meteora DLMM.
//!
//! # Reference: MeteoraAg/dlmm-sdk
//!
//! ## Price Formula
//! - `price = (1 + bin_step/10000)^bin_id` in Q64.64 format
//! - Bin step is in basis points (e.g., 25 = 0.25%)
//!
//! ## Swap Logic
//! - Liquidity is concentrated in discrete bins
//! - Swaps iterate through bins until input exhausted
//! - Each bin has a fixed price; slippage only from crossing bins

use std::collections::BTreeMap;
use uint::construct_uint;

// Define U256 for high-precision intermediate calculations
construct_uint! {
    pub struct U256(4);
}

// =============================================================================
// CONSTANTS
// =============================================================================

/// Q64.64 scale factor
pub const Q64: u128 = 1u128 << 64;

/// Scale offset for fixed-point math
pub const SCALE_OFFSET: u32 = 64;

/// Basis points max (10000 = 100%)
pub const BASIS_POINT_MAX: u64 = 10_000;

/// Fee rate denominator (for fee calculations)
pub const FEE_RATE_DENOMINATOR: u64 = 1_000_000_000;

// =============================================================================
// PRICE MATH (from MeteoraAg/dlmm-sdk)
// =============================================================================

/// Calculate price at a given bin ID.
/// Formula: price = (1 + bin_step/10000)^bin_id in Q64.64
///
/// Reference: MeteoraAg/dlmm-sdk lb_clmm::math::price_math::get_price_from_id
pub fn get_price_from_id(bin_id: i32, bin_step: u16) -> Option<u128> {
    // base = 1 + bin_step / 10000 in Q64.64
    // For bin_step = 25: base = 1.0025
    let bin_step_fraction = (bin_step as u128)
        .checked_mul(Q64)?
        .checked_div(BASIS_POINT_MAX as u128)?;
    
    let base = Q64.checked_add(bin_step_fraction)?;
    
    // Compute base^bin_id using binary exponentiation
    pow_fixed(base, bin_id)
}

/// Fixed-point exponentiation: base^exp where base is Q64.64
/// Handles negative exponents by computing 1/base^|exp|
/// Uses U256 for intermediate calculations to avoid overflow.
fn pow_fixed(base: u128, exp: i32) -> Option<u128> {
    if exp == 0 {
        return Some(Q64); // base^0 = 1
    }
    
    // Sanity check: base should be reasonable
    if base == 0 {
        return Some(0);
    }
    
    let (exp_abs, is_negative) = if exp < 0 {
        ((-exp) as u32, true)
    } else {
        (exp as u32, false)
    };
    
    // Limit exponent to prevent overflow (bin_id typically -443636 to 443636)
    // For very large exponents, result would overflow or underflow anyway
    if exp_abs > 500000 {
        return None;
    }
    
    let q64_256 = U256::from(Q64);
    
    // Binary exponentiation using U256
    let mut result = U256::from(Q64);
    let mut current_base = U256::from(base);
    let mut remaining_exp = exp_abs;
    
    while remaining_exp > 0 {
        if remaining_exp & 1 == 1 {
            // result = result * current_base / Q64
            result = result.saturating_mul(current_base) / q64_256;
            // Check for underflow to zero
            if result.is_zero() {
                return if is_negative { None } else { Some(0) };
            }
        }
        // current_base = current_base * current_base / Q64
        current_base = current_base.saturating_mul(current_base) / q64_256;
        // Check for overflow (value too large for u128)
        if current_base > U256::from(u128::MAX) {
            return None;
        }
        remaining_exp >>= 1;
    }
    
    // Convert back to u128
    if is_negative {
        // For negative exponents: 1/result = Q64^2 / result
        if result.is_zero() {
            return None; // Division by zero
        }
        let q64_squared = q64_256.saturating_mul(q64_256);
        let inv = q64_squared / result;
        if inv > U256::from(u128::MAX) {
            return None;
        }
        Some(inv.as_u128())
    } else {
        if result > U256::from(u128::MAX) {
            return None;
        }
        Some(result.as_u128())
    }
}

/// Get bin ID from price (inverse of get_price_from_id).
/// Uses binary search since log in fixed-point is complex.
pub fn get_id_from_price(price_x64: u128, bin_step: u16) -> Option<i32> {
    // Binary search for bin_id
    let mut low: i32 = -443636; // Approximate min (from CLMM)
    let mut high: i32 = 443636;  // Approximate max
    
    while low < high {
        let mid = low + (high - low) / 2;
        let price_at_mid = get_price_from_id(mid, bin_step)?;
        
        if price_at_mid <= price_x64 {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    
    Some(low - 1)
}

// =============================================================================
// BIN STATE
// =============================================================================

/// Represents a single liquidity bin.
#[derive(Debug, Clone, Copy)]
pub struct Bin {
    /// Bin ID (determines price)
    pub id: i32,
    /// Amount of token X in this bin
    pub amount_x: u64,
    /// Amount of token Y in this bin
    pub amount_y: u64,
    /// Price at this bin (cached, Q64.64)
    pub price_x64: u128,
}

impl Bin {
    /// Create a new bin.
    pub fn new(id: i32, amount_x: u64, amount_y: u64, bin_step: u16) -> Option<Self> {
        let price_x64 = get_price_from_id(id, bin_step)?;
        Some(Self {
            id,
            amount_x,
            amount_y,
            price_x64,
        })
    }
    
    /// Check if this bin has liquidity for the given swap direction.
    pub fn has_liquidity(&self, swap_for_y: bool) -> bool {
        if swap_for_y {
            self.amount_y > 0
        } else {
            self.amount_x > 0
        }
    }
    
    /// Get the amount out for a given amount in at this bin's price.
    ///
    /// Reference: MeteoraAg/dlmm-sdk bin.rs::get_amount_out
    pub fn get_amount_out(&self, amount_in: u64, swap_for_y: bool) -> u64 {
        if amount_in == 0 {
            return 0;
        }
        
        if swap_for_y {
            // Swapping X for Y: amount_out = amount_in * price
            let amount_out_128 = (amount_in as u128)
                .saturating_mul(self.price_x64)
                / Q64;
            
            // Cap at available Y
            (amount_out_128 as u64).min(self.amount_y)
        } else {
            // Swapping Y for X: amount_out = amount_in / price
            let amount_out_128 = (amount_in as u128)
                .saturating_mul(Q64)
                / self.price_x64.max(1);
            
            // Cap at available X
            (amount_out_128 as u64).min(self.amount_x)
        }
    }
    
    /// Get the amount in required for a given amount out at this bin's price.
    pub fn get_amount_in(&self, amount_out: u64, swap_for_y: bool) -> u64 {
        if amount_out == 0 {
            return 0;
        }
        
        if swap_for_y {
            // Want Y, pay X: amount_in = amount_out / price
            let amount_in_128 = (amount_out as u128)
                .saturating_mul(Q64)
                / self.price_x64.max(1);
            
            (amount_in_128 as u64).saturating_add(1) // Round up
        } else {
            // Want X, pay Y: amount_in = amount_out * price
            let amount_in_128 = (amount_out as u128)
                .saturating_mul(self.price_x64)
                / Q64;
            
            (amount_in_128 as u64).saturating_add(1) // Round up
        }
    }
    
    /// Perform a swap within this bin, returning (amount_out, consumed_in).
    ///
    /// Reference: MeteoraAg/dlmm-sdk bin.rs::swap
    pub fn swap(&mut self, amount_in: u64, swap_for_y: bool) -> (u64, u64) {
        if amount_in == 0 {
            return (0, 0);
        }
        
        if swap_for_y {
            // Swapping X for Y
            let max_y_out = self.amount_y;
            let y_out = self.get_amount_out(amount_in, true).min(max_y_out);
            
            if y_out == 0 {
                return (0, 0);
            }
            
            // Calculate actual X consumed
            let x_consumed = if y_out == max_y_out {
                // Consumed all Y, compute how much X that took
                self.get_amount_in(y_out, true)
            } else {
                amount_in
            };
            
            // Update bin state
            self.amount_x = self.amount_x.saturating_add(x_consumed);
            self.amount_y = self.amount_y.saturating_sub(y_out);
            
            (y_out, x_consumed)
        } else {
            // Swapping Y for X
            let max_x_out = self.amount_x;
            let x_out = self.get_amount_out(amount_in, false).min(max_x_out);
            
            if x_out == 0 {
                return (0, 0);
            }
            
            // Calculate actual Y consumed
            let y_consumed = if x_out == max_x_out {
                self.get_amount_in(x_out, false)
            } else {
                amount_in
            };
            
            // Update bin state
            self.amount_y = self.amount_y.saturating_add(y_consumed);
            self.amount_x = self.amount_x.saturating_sub(x_out);
            
            (x_out, y_consumed)
        }
    }
}

// =============================================================================
// DLMM POOL STATE
// =============================================================================

/// DLMM pool state for simulation.
///
/// Contains bin data and fee configuration.
#[derive(Debug, Clone)]
pub struct DlmmPoolState {
    /// Bin step in basis points (e.g., 25 = 0.25%)
    pub bin_step: u16,
    /// Active bin ID (where current price is)
    pub active_bin_id: i32,
    /// Base fee rate in millionths (e.g., 2500 = 0.25%)
    pub base_fee_rate: u32,
    /// Variable fee rate (from volatility, in millionths)
    pub variable_fee_rate: u32,
    /// All bins with liquidity, indexed by bin ID
    pub bins: BTreeMap<i32, Bin>,
}

impl DlmmPoolState {
    /// Create a new DLMM pool.
    pub fn new(bin_step: u16, active_bin_id: i32, base_fee_rate: u32) -> Self {
        Self {
            bin_step,
            active_bin_id,
            base_fee_rate,
            variable_fee_rate: 0,
            bins: BTreeMap::new(),
        }
    }
    
    /// Add a bin with liquidity.
    pub fn add_bin(&mut self, id: i32, amount_x: u64, amount_y: u64) {
        if let Some(bin) = Bin::new(id, amount_x, amount_y, self.bin_step) {
            self.bins.insert(id, bin);
        }
    }
    
    /// Get total fee rate (base + variable).
    pub fn total_fee_rate(&self) -> u32 {
        self.base_fee_rate.saturating_add(self.variable_fee_rate)
    }
    
    /// Get current price from active bin.
    pub fn price(&self) -> f64 {
        let price_x64 = get_price_from_id(self.active_bin_id, self.bin_step).unwrap_or(Q64);
        (price_x64 as f64) / (Q64 as f64)
    }
    
    /// Advance to the next bin in the swap direction.
    fn advance_active_bin(&mut self, swap_for_y: bool) -> Option<i32> {
        let next_id = if swap_for_y {
            // Price decreasing (selling X for Y), move to lower bins
            self.active_bin_id - 1
        } else {
            // Price increasing (selling Y for X), move to higher bins
            self.active_bin_id + 1
        };
        
        // Check if next bin exists with liquidity
        if self.bins.contains_key(&next_id) {
            self.active_bin_id = next_id;
            Some(next_id)
        } else {
            // Find next bin with liquidity in the direction
            let next_bin_id = if swap_for_y {
                self.bins.range(..self.active_bin_id).next_back().map(|(&id, _)| id)
            } else {
                self.bins.range((self.active_bin_id + 1)..).next().map(|(&id, _)| id)
            };
            
            if let Some(id) = next_bin_id {
                self.active_bin_id = id;
            }
            next_bin_id
        }
    }
    
    /// Simulate a swap on this DLMM pool.
    ///
    /// This implements bin-by-bin iteration from:
    /// MeteoraAg/dlmm-sdk lb_clmm::instructions::swap
    ///
    /// # Arguments
    /// * `amount_in` - Input amount
    /// * `swap_for_y` - true = swap X for Y, false = swap Y for X
    ///
    /// # Returns
    /// * `(total_in, total_out, new_state)` - Consumed input, produced output, new state
    pub fn simulate_swap(
        &self,
        amount_in: u64,
        swap_for_y: bool,
    ) -> (u64, u64, DlmmPoolState) {
        if amount_in == 0 {
            return (0, 0, self.clone());
        }
        
        // Apply fees
        let fee_rate = self.total_fee_rate();
        let fee_amount = ((amount_in as u128) * (fee_rate as u128) / 1_000_000_000) as u64;
        let amount_in_after_fee = amount_in.saturating_sub(fee_amount);
        
        if amount_in_after_fee == 0 {
            return (0, 0, self.clone());
        }
        
        let mut new_state = self.clone();
        let mut remaining_in = amount_in_after_fee;
        let mut total_out: u64 = 0;
        let mut total_consumed: u64 = 0;
        
        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 100;
        
        while remaining_in > 0 && iterations < MAX_ITERATIONS {
            iterations += 1;
            
            // Get current active bin
            let active_id = new_state.active_bin_id;
            let bin = match new_state.bins.get_mut(&active_id) {
                Some(b) => b,
                None => {
                    // No bin at current position, try to advance
                    if new_state.advance_active_bin(swap_for_y).is_none() {
                        break; // No more bins
                    }
                    continue;
                }
            };
            
            // Check if bin has liquidity
            if !bin.has_liquidity(swap_for_y) {
                if new_state.advance_active_bin(swap_for_y).is_none() {
                    break;
                }
                continue;
            }
            
            // Swap within this bin
            let (amount_out, consumed) = bin.swap(remaining_in, swap_for_y);
            
            if consumed == 0 {
                if new_state.advance_active_bin(swap_for_y).is_none() {
                    break;
                }
                continue;
            }
            
            remaining_in = remaining_in.saturating_sub(consumed);
            total_out = total_out.saturating_add(amount_out);
            total_consumed = total_consumed.saturating_add(consumed);
            
            // Check if bin is exhausted
            if !bin.has_liquidity(swap_for_y) {
                if new_state.advance_active_bin(swap_for_y).is_none() {
                    break;
                }
            }
        }
        
        // Total consumed includes fee
        let total_in_with_fee = total_consumed.saturating_add(fee_amount);
        
        (total_in_with_fee, total_out, new_state)
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_price_from_id() {
        // bin_id = 0 should give price = 1.0
        let price_at_0 = get_price_from_id(0, 25).unwrap();
        assert!((price_at_0 as i128 - Q64 as i128).abs() < 1000, "Price at 0 should be ~1.0");
        
        // Price increases with bin_id
        let price_at_100 = get_price_from_id(100, 25).unwrap();
        assert!(price_at_100 > price_at_0, "Price should increase with bin_id");
        
        // Price decreases for negative bin_id
        let price_at_neg100 = get_price_from_id(-100, 25).unwrap();
        assert!(price_at_neg100 < price_at_0, "Price should decrease for negative bin_id");
    }

    #[test]
    fn test_bin_swap() {
        // Create a bin with 100 X and 200 Y at price ~1.0
        let mut bin = Bin::new(0, 100_000_000, 200_000_000, 25).unwrap();
        
        // Swap 50 X for Y
        let (y_out, x_consumed) = bin.swap(50_000_000, true);
        
        assert!(y_out > 0, "Should get Y output");
        assert!(x_consumed > 0, "Should consume X input");
        assert!(bin.amount_x > 100_000_000, "X should increase");
        assert!(bin.amount_y < 200_000_000, "Y should decrease");
    }

    #[test]
    fn test_dlmm_pool_swap() {
        let mut pool = DlmmPoolState::new(
            25,    // bin_step
            0,     // active_bin_id
            2500,  // 0.25% base fee
        );
        
        // Add bins around active
        pool.add_bin(-1, 100_000_000, 0);        // Only X
        pool.add_bin(0, 100_000_000, 100_000_000); // Both X and Y
        pool.add_bin(1, 0, 100_000_000);          // Only Y
        
        // Swap X for Y
        let (total_in, total_out, new_state) = pool.simulate_swap(50_000_000, true);
        
        assert!(total_in > 0, "Should consume input");
        assert!(total_out > 0, "Should produce output");
        assert!(new_state.active_bin_id <= pool.active_bin_id, "Active bin should stay or move down");
    }
}
