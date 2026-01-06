//! Arbitrage detection
//!
//! Simple cross-pool arb: find where to buy cheap, sell expensive.

use crate::math::{calculate_swap, apply_slippage, flash_loan_fee};
use crate::pools::{DexType, PairKey, Pool, PoolRegistry};
use solana_sdk::pubkey::Pubkey;
use std::time::Instant;

/// Detected arbitrage opportunity
#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    /// When detected
    pub detected_at: Instant,
    /// Token pair being arbitraged
    pub pair: PairKey,
    /// Pool to buy on (cheaper)
    pub buy_pool: Pubkey,
    pub buy_dex: DexType,
    /// Pool to sell on (more expensive)
    pub sell_pool: Pubkey,
    pub sell_dex: DexType,
    /// Amount to borrow (in quote token, e.g., SOL lamports)
    pub borrow_amount: u64,
    /// Expected output from buy leg
    pub buy_output: u64,
    /// Expected output from sell leg (should be > borrow_amount)
    pub sell_output: u64,
    /// Minimum acceptable buy output (with slippage)
    pub min_buy_output: u64,
    /// Minimum acceptable sell output (with slippage)
    pub min_sell_output: u64,
    /// Expected profit in quote token (lamports)
    pub expected_profit: u64,
    /// Direction: true = buy A with B, sell A for B
    pub buy_a_sell_a: bool,
    /// Quote mint (what we borrow and repay)
    pub quote_mint: Pubkey,
    /// Base mint (what we buy and sell)
    pub base_mint: Pubkey,
}

/// Arb detector configuration
pub struct ArbDetector {
    pub min_profit_lamports: u64,
    pub slippage_bps: u16,
    /// Test amount for price comparison (1 SOL equivalent)
    pub test_amount: u64,
    /// Actual trade sizes to try
    pub trade_amounts: Vec<u64>,
}

impl Default for ArbDetector {
    fn default() -> Self {
        Self {
            min_profit_lamports: 100_000, // 0.0001 SOL
            slippage_bps: 150,            // 1.5%
            test_amount: 1_000_000_000,   // 1 SOL
            trade_amounts: vec![
                500_000_000,     // 0.5 SOL
                1_000_000_000,   // 1 SOL
                2_000_000_000,   // 2 SOL
                5_000_000_000,   // 5 SOL
            ],
        }
    }
}

impl ArbDetector {
    pub fn new(min_profit_lamports: u64, slippage_bps: u16) -> Self {
        Self {
            min_profit_lamports,
            slippage_bps,
            ..Default::default()
        }
    }

    /// Check for arb opportunities on a pair after state update
    pub fn check_pair(
        &self,
        registry: &PoolRegistry,
        pair: &PairKey,
    ) -> Option<ArbOpportunity> {
        let pools = registry.pools_for_pair(pair);
        
        if pools.len() < 2 {
            return None; // Need at least 2 pools for arb
        }

        // Filter pools with liquidity
        let valid_pools: Vec<_> = pools.into_iter()
            .filter(|p| p.reserve_a > 0 && p.reserve_b > 0)
            .collect();

        if valid_pools.len() < 2 {
            return None;
        }

        // Try both directions: A→B→A or B→A→B
        let opp_a = self.find_best_arb(&valid_pools, true, pair);
        let opp_b = self.find_best_arb(&valid_pools, false, pair);

        // Return the more profitable one
        match (opp_a, opp_b) {
            (Some(a), Some(b)) => {
                if a.expected_profit >= b.expected_profit { Some(a) } else { Some(b) }
            }
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Find best arb for a given direction
    /// buy_a_sell_a: true = borrow B, buy A, sell A for B
    fn find_best_arb(
        &self,
        pools: &[&Pool],
        buy_a_sell_a: bool,
        pair: &PairKey,
    ) -> Option<ArbOpportunity> {
        // Find best buy pool (most output for input)
        // Find best sell pool (most output for the tokens we bought)
        
        let mut best: Option<ArbOpportunity> = None;

        for &trade_amount in &self.trade_amounts {
            if let Some(opp) = self.evaluate_trade(pools, trade_amount, buy_a_sell_a, pair) {
                if opp.expected_profit >= self.min_profit_lamports {
                    if best.as_ref().map(|b| opp.expected_profit > b.expected_profit).unwrap_or(true) {
                        best = Some(opp);
                    }
                }
            }
        }

        best
    }

    /// Evaluate a specific trade amount
    fn evaluate_trade(
        &self,
        pools: &[&Pool],
        borrow_amount: u64,
        buy_a_sell_a: bool,
        pair: &PairKey,
    ) -> Option<ArbOpportunity> {
        // Find best pool to buy on
        let mut best_buy: Option<(&Pool, u64)> = None;
        for pool in pools {
            // buy_a_sell_a: swap B→A (a_to_b = false)
            let output = calculate_swap(pool, borrow_amount, !buy_a_sell_a)?;
            if best_buy.as_ref().map(|(_, o)| output > *o).unwrap_or(true) {
                best_buy = Some((pool, output));
            }
        }
        let (buy_pool, buy_output) = best_buy?;

        // Find best pool to sell on (excluding buy pool)
        let mut best_sell: Option<(&Pool, u64)> = None;
        for pool in pools {
            if pool.address == buy_pool.address {
                continue; // Can't use same pool
            }
            // Sell A→B (a_to_b = true if buy_a_sell_a)
            let output = calculate_swap(pool, buy_output, buy_a_sell_a)?;
            if best_sell.as_ref().map(|(_, o)| output > *o).unwrap_or(true) {
                best_sell = Some((pool, output));
            }
        }
        let (sell_pool, sell_output) = best_sell?;

        // Calculate profit
        let flash_fee = flash_loan_fee(borrow_amount);
        let repay_amount = borrow_amount + flash_fee;

        if sell_output <= repay_amount {
            return None; // Not profitable
        }

        let expected_profit = sell_output - repay_amount;
        
        if expected_profit < self.min_profit_lamports {
            return None;
        }

        // Apply slippage to get minimum acceptable outputs
        let min_buy_output = apply_slippage(buy_output, self.slippage_bps);
        let min_sell_output = apply_slippage(sell_output, self.slippage_bps);

        // Verify still profitable after slippage
        if min_sell_output <= repay_amount {
            return None;
        }

        // Determine quote/base mints
        let (quote_mint, base_mint) = if buy_a_sell_a {
            // Borrow B, buy A, sell A for B
            (pair.1, pair.0)
        } else {
            // Borrow A, buy B, sell B for A
            (pair.0, pair.1)
        };

        Some(ArbOpportunity {
            detected_at: Instant::now(),
            pair: *pair,
            buy_pool: buy_pool.address,
            buy_dex: buy_pool.dex,
            sell_pool: sell_pool.address,
            sell_dex: sell_pool.dex,
            borrow_amount,
            buy_output,
            sell_output,
            min_buy_output,
            min_sell_output,
            expected_profit,
            buy_a_sell_a,
            quote_mint,
            base_mint,
        })
    }
}

/// Quick stats for logging
impl ArbOpportunity {
    pub fn profit_sol(&self) -> f64 {
        self.expected_profit as f64 / 1_000_000_000.0
    }

    pub fn spread_bps(&self) -> u16 {
        if self.borrow_amount == 0 {
            return 0;
        }
        ((self.sell_output - self.borrow_amount) * 10000 / self.borrow_amount) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pools::Pool;

    fn make_pool(addr: u8, reserve_a: u64, reserve_b: u64) -> Pool {
        Pool {
            address: Pubkey::new_from_array([addr; 32]),
            dex: DexType::RaydiumAmm,
            mint_a: Pubkey::new_from_array([1; 32]),
            mint_b: Pubkey::new_from_array([2; 32]),
            vault_a: Pubkey::new_from_array([addr + 100; 32]),
            vault_b: Pubkey::new_from_array([addr + 200; 32]),
            decimals_a: 9,
            decimals_b: 6,
            reserve_a,
            reserve_b,
            fee_bps: 25,
            tick_spacing: None,
            current_tick: None,
            bin_step: None,
            active_bin_id: None,
        }
    }

    #[test]
    fn test_arb_detection() {
        // Pool 1: 1000 SOL, 100,000 USDC (price = $100)
        let pool1 = make_pool(1, 1_000_000_000_000, 100_000_000_000);
        // Pool 2: 1000 SOL, 105,000 USDC (price = $105) - SOL is more expensive here
        let pool2 = make_pool(2, 1_000_000_000_000, 105_000_000_000);
        
        let pools: Vec<&Pool> = vec![&pool1, &pool2];
        let pair = PairKey::new(pool1.mint_a, pool1.mint_b);
        
        let detector = ArbDetector::new(1000, 100);
        
        // Should find arb: buy SOL cheap on pool1, sell expensive on pool2
        let opp = detector.find_best_arb(&pools, true, &pair);
        
        assert!(opp.is_some());
        let opp = opp.unwrap();
        assert!(opp.expected_profit > 0);
        
        // Buy pool should be pool1 (cheaper SOL)
        // Sell pool should be pool2 (more expensive SOL)
        println!("Profit: {} lamports", opp.expected_profit);
    }
}
