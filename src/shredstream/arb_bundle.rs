//! Arbitrage Transaction Builder
//!
//! Constructs atomic arbitrage transactions for cross-pool price divergence.
//!
//! # Pure Arbitrage Strategy
//!
//! When we detect price divergence between pools:
//! 1. Flash loan the anchor token (SOL/USDC)
//! 2. Buy on the cheap pool
//! 3. Sell on the expensive pool
//! 4. Repay flash loan + fee
//! 5. Keep profit
//!
//! This is submitted as a **single-TX Jito bundle** for priority inclusion.

use super::evaluator::{ArbOpportunity, PoolSwapState};
use super::programs::DexType;
use crate::constants::{
    ATA_PROGRAM, SYSTEM_PROGRAM, TOKEN_PROGRAM, TOKEN_2022_PROGRAM, WSOL_MINT,
    MIN_PAYER_BALANCE_LAMPORTS,
    ORCA_MIN_SQRT_PRICE, ORCA_MAX_SQRT_PRICE,
    RAYDIUM_CLMM_MIN_SQRT_PRICE, RAYDIUM_CLMM_MAX_SQRT_PRICE,
    PANCAKE_CLMM_MIN_SQRT_PRICE, PANCAKE_CLMM_MAX_SQRT_PRICE,
    // Bundle builder defaults
    DEFAULT_COMPUTE_UNITS, DEFAULT_COMPUTE_UNIT_PRICE, DEFAULT_BASE_TIP_LAMPORTS,
    DEFAULT_TIP_PROFIT_SHARE_BPS, DEFAULT_MAX_TIP_PROFIT_BPS, DEFAULT_SLIPPAGE_BPS,
    DEFAULT_TIP_FLOOR_MULTIPLIER,
};
use crate::executor::FlashLoanReserve;
use crate::jito::random_tip_account;
use crate::pool_discovery::{
    derive_dlmm_bin_arrays, derive_raydium_clmm_tick_arrays, derive_whirlpool_tick_arrays,
};
use crate::tip_monitor::TipFloorCache;
use anyhow::{anyhow, bail, Result};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_message::AddressLookupTableAccount;
use solana_sdk::hash::Hash;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::message::{v0, VersionedMessage};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::VersionedTransaction;
use solana_system_interface::instruction as system_instruction;
// Use FxHashMap for faster lookups
use rustc_hash::FxHashMap as HashMap;
use std::sync::Arc;

/// An arbitrage bundle ready for Jito submission (single TX)
#[derive(Debug, Clone)]
pub struct ArbBundle {
    /// Unique bundle ID
    pub id: String,
    /// The arbitrage transaction
    pub arb_tx: VersionedTransaction,
    /// Expected profit in lamports
    pub expected_profit_lamports: u64,
    /// Tip amount included
    pub tip_lamports: u64,
    /// The opportunity this bundle was built from
    pub opportunity_id: String,
}

impl ArbBundle {
    /// Get transaction as a vector for Jito submission
    pub fn transactions(&self) -> Vec<VersionedTransaction> {
        vec![self.arb_tx.clone()]
    }
}

/// Configuration for the bundle builder
#[derive(Clone, Debug)]
pub struct BundleBuilderConfig {
    /// Compute units to request
    pub compute_units: u32,
    /// Compute unit price in micro-lamports
    pub compute_unit_price: u64,
    /// Base tip (minimum)
    pub base_tip_lamports: u64,
    /// Percentage of profit to tip (in bps, e.g., 5000 = 50%)
    pub tip_profit_share_bps: u64,
    /// Maximum tip as percentage of profit
    pub max_tip_profit_bps: u64,
    /// Slippage tolerance in bps
    pub slippage_bps: u64,
    /// Multiplier over tip floor (e.g., 1.5 = 50% above floor)
    pub tip_floor_multiplier: f64,
}

impl Default for BundleBuilderConfig {
    fn default() -> Self {
        Self {
            compute_units: DEFAULT_COMPUTE_UNITS,
            compute_unit_price: DEFAULT_COMPUTE_UNIT_PRICE,
            base_tip_lamports: DEFAULT_BASE_TIP_LAMPORTS,
            tip_profit_share_bps: DEFAULT_TIP_PROFIT_SHARE_BPS,
            max_tip_profit_bps: DEFAULT_MAX_TIP_PROFIT_BPS,
            slippage_bps: DEFAULT_SLIPPAGE_BPS,
            tip_floor_multiplier: DEFAULT_TIP_FLOOR_MULTIPLIER,
        }
    }
}

/// A single leg of the arbitrage with data needed to build a swap instruction
#[derive(Debug, Clone)]
pub struct ArbLeg {
    pub pool: Pubkey,
    pub dex: DexType,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    /// Expected output from simulation (in output token base units)
    pub estimated_output: u64,
    /// Minimum acceptable output with slippage applied (pre-computed by evaluator)
    pub min_output: u64,
    pub pool_state: PoolSwapState,
}

/// Bundle builder for arbitrage bundles
pub struct ArbBundleBuilder {
    config: BundleBuilderConfig,
    payer: Arc<Keypair>,
    flash_loan_reserves: HashMap<Pubkey, FlashLoanReserve>,
    lookup_tables: Vec<AddressLookupTableAccount>,
    tip_floor_cache: Option<Arc<TipFloorCache>>,
    /// Cached payer SOL balance for pre-flight checks (updated externally)
    payer_balance: std::sync::atomic::AtomicU64,
}

impl ArbBundleBuilder {
    pub fn new(
        config: BundleBuilderConfig,
        payer: Arc<Keypair>,
        flash_loan_reserves: HashMap<Pubkey, FlashLoanReserve>,
        lookup_tables: Vec<AddressLookupTableAccount>,
    ) -> Self {
        Self {
            config,
            payer,
            flash_loan_reserves,
            lookup_tables,
            tip_floor_cache: None,
            payer_balance: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Update cached payer balance (should be called periodically from main loop)
    pub fn update_payer_balance(&self, balance: u64) {
        self.payer_balance.store(balance, std::sync::atomic::Ordering::Relaxed);
    }

    /// Get cached payer balance
    pub fn payer_balance(&self) -> u64 {
        self.payer_balance.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get payer pubkey (for balance queries)
    pub fn payer_pubkey(&self) -> Pubkey {
        self.payer.pubkey()
    }

    /// Check if payer has sufficient balance for a bundle
    pub fn has_sufficient_balance(&self, tip_lamports: u64) -> bool {
        let balance = self.payer_balance();
        let required = MIN_PAYER_BALANCE_LAMPORTS.saturating_add(tip_lamports);
        balance >= required
    }

    /// Set the tip floor cache for dynamic tipping
    pub fn with_tip_floor_cache(mut self, cache: Arc<TipFloorCache>) -> Self {
        self.tip_floor_cache = Some(cache);
        self
    }

    /// Set tip floor cache after construction
    pub fn set_tip_floor_cache(&mut self, cache: Arc<TipFloorCache>) {
        self.tip_floor_cache = Some(cache);
    }

    /// Build an arbitrage bundle from a cross-pool price discrepancy
    pub fn build(&self, opportunity: &ArbOpportunity, blockhash: Hash) -> Result<ArbBundle> {
        let reserve = self
            .flash_loan_reserves
            .get(&opportunity.borrow_mint)
            .ok_or_else(|| anyhow!("No flash loan reserve for {}", opportunity.borrow_mint))?;

        // Pre-calculate tip for balance check
        let tip = self.calculate_tip(opportunity.expected_profit_lamports);

        // Pre-flight balance check
        if !self.has_sufficient_balance(tip) {
            bail!(
                "Insufficient payer balance: have {} lamports, need {} + {} tip",
                self.payer_balance(),
                MIN_PAYER_BALANCE_LAMPORTS,
                tip
            );
        }

        let payer_pubkey = self.payer.pubkey();
        let borrow_ata = derive_ata(&payer_pubkey, &opportunity.borrow_mint);
        
        // Track detection-to-build latency
        let detection_age_us = opportunity.detected_at.elapsed().as_micros();

        // LOG: Show what we're building
        tracing::info!(
            opp_id = %opportunity.id,
            buy_pool = %opportunity.buy_pool,
            buy_dex = ?opportunity.buy_dex,
            sell_pool = %opportunity.sell_pool,
            sell_dex = ?opportunity.sell_dex,
            borrow_mint = %opportunity.borrow_mint,
            base_mint = %opportunity.base_mint,
            trade_amount = opportunity.trade_amount,
            leg1_output = opportunity.leg1_output,
            leg2_output = opportunity.leg2_output,
            expected_profit = opportunity.expected_profit_lamports,
            detection_age_us = detection_age_us,
            flash_reserve = %reserve.reserve,
            flash_supply = %reserve.borrow_liquidity_supply,
            "BUILDING ARB BUNDLE"
        );

        let arb_tx = self.build_arb_transaction(opportunity, reserve, borrow_ata, blockhash)?;

        Ok(ArbBundle {
            id: format!("arb_{}", opportunity.id),
            arb_tx,
            expected_profit_lamports: opportunity.expected_profit_lamports,
            tip_lamports: tip,
            opportunity_id: opportunity.id.clone(),
        })
    }

    /// Build the arbitrage transaction
    ///
    /// Structure: flash borrow → swap1 (buy cheap) → swap2 (sell high) → flash repay → sync_native → tip
    fn build_arb_transaction(
        &self,
        opportunity: &ArbOpportunity,
        reserve: &FlashLoanReserve,
        borrow_ata: Pubkey,
        blockhash: Hash,
    ) -> Result<VersionedTransaction> {
        let payer_pubkey = self.payer.pubkey();

        // Determine token programs for each mint based on pool state
        // Use buy pool's token programs as source of truth for the pair
        let borrow_token_program = if opportunity.borrow_mint == opportunity.buy_pool_state.token_mint_a {
            opportunity.buy_pool_state.token_program_a
        } else {
            opportunity.buy_pool_state.token_program_b
        };
        
        let base_token_program = if opportunity.base_mint == opportunity.buy_pool_state.token_mint_a {
            opportunity.buy_pool_state.token_program_a
        } else {
            opportunity.buy_pool_state.token_program_b
        };

        // Pre-allocate: 2 compute + 2 ATA + 1 borrow + 2 swaps + 1 repay + 1 sync + 1 tip + 1 close = 12
        let mut instructions = Vec::with_capacity(12);

        // 1. Compute budget (use 600K for safety with CLMM swaps)
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(
            self.config.compute_units.max(600_000),
        ));
        instructions.push(ComputeBudgetInstruction::set_compute_unit_price(
            self.config.compute_unit_price,
        ));

        // 2. Create ATAs (idempotent) - use correct token program per mint
        instructions.push(create_ata_idempotent(
            &payer_pubkey,
            &payer_pubkey,
            &opportunity.borrow_mint,
            &borrow_token_program,
        ));

        if opportunity.base_mint != opportunity.borrow_mint {
            instructions.push(create_ata_idempotent(
                &payer_pubkey,
                &payer_pubkey,
                &opportunity.base_mint,
                &base_token_program,
            ));
        }

        // Track instruction index for flash loan repay reference
        let borrow_ix_index = instructions.len() as u8;
        tracing::debug!(borrow_ix_index, "Flash borrow at instruction index");

        // 3. Flash borrow
        instructions.push(self.build_borrow_instruction(
            reserve,
            opportunity.trade_amount,
            borrow_ata,
        ));

        // 4. Swap leg 1: Buy on cheaper pool (quote -> base)
        let buy_leg = ArbLeg {
            pool: opportunity.buy_pool,
            dex: opportunity.buy_dex,
            input_mint: opportunity.borrow_mint,
            output_mint: opportunity.base_mint,
            estimated_output: opportunity.leg1_output,
            min_output: opportunity.min_leg1_output,
            pool_state: opportunity.buy_pool_state.clone(),
        };
        tracing::debug!(
            ix_index = instructions.len(),
            pool = %buy_leg.pool,
            dex = ?buy_leg.dex,
            input = %buy_leg.input_mint,
            output = %buy_leg.output_mint,
            amount_in = opportunity.trade_amount,
            expected_out = opportunity.leg1_output,
            vault_a = %buy_leg.pool_state.token_vault_a,
            vault_b = %buy_leg.pool_state.token_vault_b,
            "SWAP LEG 1 (buy)"
        );
        let swap1 = self.build_swap_instruction(&buy_leg, opportunity.trade_amount)?;
        instructions.push(swap1);

        // 5. Swap leg 2: Sell on expensive pool (base -> quote)
        // Use min_leg1_output as the input amount - this is the conservative estimate
        // of what we'll receive from leg 1. If we receive more, extra stays in our ATA.
        let sell_leg = ArbLeg {
            pool: opportunity.sell_pool,
            dex: opportunity.sell_dex,
            input_mint: opportunity.base_mint,
            output_mint: opportunity.borrow_mint,
            estimated_output: opportunity.leg2_output,
            min_output: opportunity.min_leg2_output,
            pool_state: opportunity.sell_pool_state.clone(),
        };
        tracing::debug!(
            ix_index = instructions.len(),
            pool = %sell_leg.pool,
            dex = ?sell_leg.dex,
            input = %sell_leg.input_mint,
            output = %sell_leg.output_mint,
            amount_in = opportunity.min_leg1_output,
            expected_out = opportunity.leg2_output,
            min_out = opportunity.min_leg2_output,
            vault_a = %sell_leg.pool_state.token_vault_a,
            vault_b = %sell_leg.pool_state.token_vault_b,
            "SWAP LEG 2 (sell)"
        );
        // Use min_leg1_output as input - ensures we don't try to swap more than we're guaranteed to receive
        let swap2 = self.build_swap_instruction(&sell_leg, opportunity.min_leg1_output)?;
        instructions.push(swap2);

        // 5.5. SyncNative if the output is wSOL (ensures balance is correct before repay)
        if opportunity.borrow_mint == WSOL_MINT {
            instructions.push(build_sync_native(&borrow_ata));
        }

        // 6. Flash repay
        tracing::debug!(ix_index = instructions.len(), "Flash repay at instruction index");
        instructions.push(self.build_repay_instruction(
            reserve,
            opportunity.trade_amount,
            borrow_ata,
            borrow_ix_index,
        ));

        // 7. Tip
        let tip = self.calculate_tip(opportunity.expected_profit_lamports);
        instructions.push(self.build_tip_instruction(tip));

        // 8. Close wSOL account to recover rent and unwrap profit to native SOL
        // Note: This must be after the tip since tip is paid from native SOL balance
        if opportunity.borrow_mint == WSOL_MINT {
            instructions.push(build_close_account(&borrow_ata, &payer_pubkey, &payer_pubkey));
        }

        // Build versioned message
        let message =
            v0::Message::try_compile(&payer_pubkey, &instructions, &self.lookup_tables, blockhash)?;

        let versioned_message = VersionedMessage::V0(message);
        let tx = VersionedTransaction::try_new(versioned_message, &[self.payer.as_ref()])?;

        Ok(tx)
    }

    /// Build a swap instruction for a leg
    /// 
    /// The min_output is pre-computed by the evaluator with proper slippage tolerance.
    /// This avoids double-applying slippage and ensures consistency between simulation and execution.
    fn build_swap_instruction(&self, leg: &ArbLeg, amount_in: u64) -> Result<Instruction> {
        let payer = self.payer.pubkey();
        let input_ata = derive_ata(&payer, &leg.input_mint);
        let output_ata = derive_ata(&payer, &leg.output_mint);

        // Use pre-computed min_output (slippage already applied by evaluator)
        let min_out = leg.min_output;

        tracing::debug!(
            dex = ?leg.dex,
            pool = %leg.pool,
            amount_in = amount_in,
            estimated_output = leg.estimated_output,
            min_out = min_out,
            input_mint = %leg.input_mint,
            output_mint = %leg.output_mint,
            input_ata = %input_ata,
            output_ata = %output_ata,
            token_mint_a = %leg.pool_state.token_mint_a,
            token_mint_b = %leg.pool_state.token_mint_b,
            vault_a = %leg.pool_state.token_vault_a,
            vault_b = %leg.pool_state.token_vault_b,
            "BUILD SWAP INSTRUCTION"
        );

        match leg.dex {
            DexType::RaydiumAmm => {
                self.build_raydium_amm_swap(leg, amount_in, min_out, input_ata, output_ata)
            }
            DexType::RaydiumClmm => {
                self.build_raydium_clmm_swap(leg, amount_in, min_out, input_ata, output_ata)
            }
            DexType::OrcaWhirlpool => {
                self.build_orca_whirlpool_swap(leg, amount_in, min_out, input_ata, output_ata)
            }
            DexType::MeteoraDlmm => {
                self.build_meteora_dlmm_swap(leg, amount_in, min_out, input_ata, output_ata)
            }
            DexType::MeteoraDamm => {
                self.build_meteora_damm_swap(leg, amount_in, min_out, input_ata, output_ata)
            }
            DexType::PancakeSwapClmm => {
                self.build_pancake_clmm_swap(leg, amount_in, min_out, input_ata, output_ata)
            }
        }
    }

    // ========================================================================
    // Swap Instruction Builders
    // ========================================================================

    fn build_raydium_amm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        let state = &leg.pool_state;

        // For Raydium AMM SwapBaseInV2:
        // - coin_vault and pc_vault are passed in FIXED order (coin first, then pc)
        // - user_source and user_dest change based on swap direction
        let coin_vault = state.token_vault_a;
        let pc_vault = state.token_vault_b;

        let amm_authority = state
            .amm_authority
            .ok_or_else(|| anyhow!("Raydium AMM swap requires amm_authority"))?;

        // SwapBaseInV2 (instruction 16) accounts:
        // 0. [] Token program
        // 1. [writable] AMM Account
        // 2. [] AMM authority
        // 3. [writable] AMM coin vault
        // 4. [writable] AMM pc vault
        // 5. [writable] User source token account
        // 6. [writable] User destination token account
        // 7. [signer] User wallet
        let accounts = vec![
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new(leg.pool, false),
            AccountMeta::new_readonly(amm_authority, false),
            AccountMeta::new(coin_vault, false),
            AccountMeta::new(pc_vault, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new_readonly(self.payer.pubkey(), true),
        ];

        let mut data = Vec::with_capacity(17);
        data.push(16u8); // swap_base_in_v2
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());

        Ok(Instruction {
            program_id: *super::programs::RAYDIUM_AMM,
            accounts,
            data,
        })
    }

    fn build_raydium_clmm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        let state = &leg.pool_state;

        let amm_config = state
            .amm_config
            .ok_or_else(|| anyhow!("Raydium CLMM requires amm_config"))?;
        let observation_state = state
            .observation_state
            .ok_or_else(|| anyhow!("Raydium CLMM requires observation_state"))?;

        let zero_for_one = leg.input_mint == state.token_mint_a;
        let (input_vault, output_vault) = if zero_for_one {
            (state.token_vault_a, state.token_vault_b)
        } else {
            (state.token_vault_b, state.token_vault_a)
        };

        let tick_arrays = if state.spacing > 0 {
            derive_raydium_clmm_tick_arrays(&leg.pool, state.current_tick_or_bin, state.spacing)
        } else if !state.tick_arrays.is_empty() {
            state.tick_arrays.clone()
        } else {
            bail!("Raydium CLMM requires tick arrays or spacing");
        };

        let mut accounts = vec![
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new_readonly(amm_config, false),
            AccountMeta::new(leg.pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(input_vault, false),
            AccountMeta::new(output_vault, false),
            AccountMeta::new(tick_arrays[0], false),
            AccountMeta::new(observation_state, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ];

        for tick_array in tick_arrays.iter().skip(1) {
            accounts.push(AccountMeta::new(*tick_array, false));
        }

        // Use proper sqrt_price_limit from constants
        // For Raydium CLMM: MIN + 1 when zero_for_one (selling A), MAX - 1 when !zero_for_one (buying A)
        let sqrt_price_limit: u128 = if zero_for_one {
            RAYDIUM_CLMM_MIN_SQRT_PRICE.saturating_add(1)
        } else {
            RAYDIUM_CLMM_MAX_SQRT_PRICE.saturating_sub(1)
        };

        let mut data = Vec::with_capacity(41);
        data.extend_from_slice(&super::programs::raydium_clmm::SWAP);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
        data.push(1u8); // is_base_input = true (we specify exact input)

        Ok(Instruction {
            program_id: *super::programs::RAYDIUM_CLMM,
            accounts,
            data,
        })
    }

    fn build_orca_whirlpool_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        let state = &leg.pool_state;

        let oracle = state
            .observation_state
            .ok_or_else(|| anyhow!("Orca Whirlpool requires oracle"))?;

        let tick_arrays = if state.spacing > 0 {
            derive_whirlpool_tick_arrays(&leg.pool, state.current_tick_or_bin, state.spacing)
        } else if state.tick_arrays.len() >= 3 {
            state.tick_arrays.clone()
        } else {
            bail!("Orca Whirlpool requires 3 tick_arrays or spacing");
        };

        if tick_arrays.len() < 3 {
            bail!("Orca Whirlpool requires 3 tick_arrays, got {}", tick_arrays.len());
        }

        let a_to_b = leg.input_mint == state.token_mint_a;
        let (token_owner_a, token_owner_b) = if a_to_b {
            (input_ata, output_ata)
        } else {
            (output_ata, input_ata)
        };

        let accounts = vec![
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new(leg.pool, false),
            AccountMeta::new(token_owner_a, false),
            AccountMeta::new(state.token_vault_a, false),
            AccountMeta::new(token_owner_b, false),
            AccountMeta::new(state.token_vault_b, false),
            AccountMeta::new(tick_arrays[0], false),
            AccountMeta::new(tick_arrays[1], false),
            AccountMeta::new(tick_arrays[2], false),
            AccountMeta::new_readonly(oracle, false),
        ];

        // Use proper sqrt_price_limit from constants
        // For Orca: MIN + 1 when selling (a_to_b), MAX - 1 when buying (!a_to_b)
        let sqrt_price_limit: u128 = if a_to_b {
            ORCA_MIN_SQRT_PRICE.saturating_add(1)
        } else {
            ORCA_MAX_SQRT_PRICE.saturating_sub(1)
        };

        let mut data = Vec::with_capacity(42);
        data.extend_from_slice(&super::programs::orca_whirlpool::SWAP);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
        data.push(1u8);
        data.push(if a_to_b { 1u8 } else { 0u8 });

        Ok(Instruction {
            program_id: *super::programs::ORCA_WHIRLPOOL,
            accounts,
            data,
        })
    }

    fn build_meteora_dlmm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        let state = &leg.pool_state;

        let (user_token_in, user_token_out) = (input_ata, output_ata);

        let bin_arrays = if !state.bin_arrays.is_empty() {
            state.bin_arrays.clone()
        } else {
            derive_dlmm_bin_arrays(&leg.pool, state.current_tick_or_bin)
        };

        // Use correct token programs from pool state (Token-2022 support)
        let token_program_a = state.token_program_a;
        let token_program_b = state.token_program_b;

        let mut accounts = vec![
            AccountMeta::new(leg.pool, false),
            AccountMeta::new(state.bin_array_bitmap_extension.unwrap_or(leg.pool), false),
            AccountMeta::new(state.token_vault_a, false),
            AccountMeta::new(state.token_vault_b, false),
            AccountMeta::new_readonly(state.token_mint_a, false),
            AccountMeta::new_readonly(state.token_mint_b, false),
            AccountMeta::new_readonly(token_program_a, false),  // token_x_program - uses actual program
            AccountMeta::new_readonly(token_program_b, false),  // token_y_program - uses actual program
            AccountMeta::new(self.payer.pubkey(), true),
            AccountMeta::new(user_token_in, false),
            AccountMeta::new(user_token_out, false),
            AccountMeta::new_readonly(state.observation_state.unwrap_or(leg.pool), false),
            AccountMeta::new_readonly(leg.pool, false),
            AccountMeta::new_readonly(
                Pubkey::find_program_address(
                    &[b"__event_authority"],
                    &super::programs::METEORA_DLMM,
                ).0,
                false,
            ),
            AccountMeta::new_readonly(*super::programs::METEORA_DLMM, false),
        ];

        for bin_array in &bin_arrays {
            accounts.push(AccountMeta::new(*bin_array, false));
        }

        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&super::programs::meteora_dlmm::SWAP2);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());

        Ok(Instruction {
            program_id: *super::programs::METEORA_DLMM,
            accounts,
            data,
        })
    }

    fn build_meteora_damm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        let state = &leg.pool_state;

        let pool_authority = state
            .pool_authority
            .ok_or_else(|| anyhow!("Meteora DAMM requires pool_authority"))?;

        // Determine swap direction: A->B or B->A
        // Official SwapCtx expects: token_a_vault, token_b_vault, token_a_mint, token_b_mint
        // in that fixed order (NOT based on direction), but token programs must match
        let accounts = vec![
            AccountMeta::new_readonly(pool_authority, false),  // 0: pool_authority (global PDA)
            AccountMeta::new(leg.pool, false),                 // 1: pool
            AccountMeta::new(input_ata, false),                // 2: input_token_account
            AccountMeta::new(output_ata, false),               // 3: output_token_account
            AccountMeta::new(state.token_vault_a, false),      // 4: token_a_vault
            AccountMeta::new(state.token_vault_b, false),      // 5: token_b_vault
            AccountMeta::new_readonly(state.token_mint_a, false),  // 6: token_a_mint
            AccountMeta::new_readonly(state.token_mint_b, false),  // 7: token_b_mint
            AccountMeta::new(self.payer.pubkey(), true),       // 8: payer (signer)
            AccountMeta::new_readonly(state.token_program_a, false),  // 9: token_a_program
            AccountMeta::new_readonly(state.token_program_b, false),  // 10: token_b_program
            // Note: referral_token_account (optional) is omitted - not using referrals
        ];

        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&super::programs::meteora_damm::SWAP);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());

        Ok(Instruction {
            program_id: *super::programs::METEORA_DAMM,
            accounts,
            data,
        })
    }

    fn build_pancake_clmm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        let state = &leg.pool_state;

        let amm_config = state
            .amm_config
            .ok_or_else(|| anyhow!("PancakeSwap CLMM requires amm_config"))?;
        let observation_state = state
            .observation_state
            .ok_or_else(|| anyhow!("PancakeSwap CLMM requires observation_state"))?;

        let zero_for_one = leg.input_mint == state.token_mint_a;
        let (input_vault, output_vault) = if zero_for_one {
            (state.token_vault_a, state.token_vault_b)
        } else {
            (state.token_vault_b, state.token_vault_a)
        };

        let tick_arrays = if state.spacing > 0 {
            derive_raydium_clmm_tick_arrays(&leg.pool, state.current_tick_or_bin, state.spacing)
        } else if !state.tick_arrays.is_empty() {
            state.tick_arrays.clone()
        } else {
            bail!("PancakeSwap CLMM requires tick arrays or spacing");
        };

        let mut accounts = vec![
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new_readonly(amm_config, false),
            AccountMeta::new(leg.pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(input_vault, false),
            AccountMeta::new(output_vault, false),
            AccountMeta::new(tick_arrays[0], false),
            AccountMeta::new(observation_state, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ];

        for tick_array in tick_arrays.iter().skip(1) {
            accounts.push(AccountMeta::new(*tick_array, false));
        }

        // Use proper sqrt_price_limit from constants
        let sqrt_price_limit: u128 = if zero_for_one {
            PANCAKE_CLMM_MIN_SQRT_PRICE.saturating_add(1)
        } else {
            PANCAKE_CLMM_MAX_SQRT_PRICE.saturating_sub(1)
        };

        let mut data = Vec::with_capacity(41);
        data.extend_from_slice(&super::programs::pancakeswap_clmm::SWAP);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
        data.push(1u8); // is_base_input = true

        Ok(Instruction {
            program_id: *super::programs::PANCAKESWAP_CLMM,
            accounts,
            data,
        })
    }

    // ========================================================================
    // Flash Loan Instructions
    // ========================================================================

    fn build_borrow_instruction(
        &self,
        reserve: &FlashLoanReserve,
        amount: u64,
        dest_ata: Pubkey,
    ) -> Instruction {
        // Derive lending market authority PDA
        let (lending_market_authority, _bump) = Pubkey::find_program_address(
            &[reserve.lending_market.as_ref()],
            &reserve.program_id,
        );

        tracing::info!(
            program = %reserve.program_id,
            reserve = %reserve.reserve,
            lending_market = %reserve.lending_market,
            lending_market_authority = %lending_market_authority,
            borrow_supply = %reserve.borrow_liquidity_supply,
            dest_ata = %dest_ata,
            amount = amount,
            "BUILD FLASH BORROW - instruction 19 with 7 accounts"
        );

        // FlashBorrowReserveLiquidity (instruction 19) accounts:
        // 0. [writable] Source liquidity (reserve supply)
        // 1. [writable] Destination liquidity  
        // 2. [writable] Reserve account
        // 3. [] Lending market
        // 4. [] Derived lending market authority (PDA)
        // 5. [] Instructions sysvar
        // 6. [] Token program
        let accounts = vec![
            AccountMeta::new(reserve.borrow_liquidity_supply, false),
            AccountMeta::new(dest_ata, false),
            AccountMeta::new(reserve.reserve, false),
            AccountMeta::new_readonly(reserve.lending_market, false),
            AccountMeta::new_readonly(lending_market_authority, false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ];

        let mut data = vec![19u8]; // FlashBorrowReserveLiquidity
        data.extend_from_slice(&amount.to_le_bytes());

        Instruction {
            program_id: reserve.program_id,
            accounts,
            data,
        }
    }

    fn build_repay_instruction(
        &self,
        reserve: &FlashLoanReserve,
        amount: u64,
        source_ata: Pubkey,
        borrow_ix_index: u8,
    ) -> Instruction {
        tracing::info!(
            program = %reserve.program_id,
            reserve = %reserve.reserve,
            repay_supply = %reserve.repay_liquidity_supply,
            fee_receiver = %reserve.repay_fee_receiver,
            host_fee = %reserve.host_fee_receiver,
            source_ata = %source_ata,
            amount = amount,
            borrow_ix_index = borrow_ix_index,
            "BUILD FLASH REPAY - instruction 20 with 9 accounts"
        );

        // FlashRepayReserveLiquidity (instruction 20) accounts:
        // 0. [writable] Source liquidity (user's token account)
        // 1. [writable] Destination liquidity (reserve supply)
        // 2. [writable] Flash loan fee receiver
        // 3. [writable] Host fee receiver
        // 4. [writable] Reserve account
        // 5. [] Lending market
        // 6. [signer] User transfer authority
        // 7. [] Instructions sysvar
        // 8. [] Token program
        let accounts = vec![
            AccountMeta::new(source_ata, false),
            AccountMeta::new(reserve.repay_liquidity_supply, false),
            AccountMeta::new(reserve.repay_fee_receiver, false),
            AccountMeta::new(reserve.host_fee_receiver, false),
            AccountMeta::new(reserve.reserve, false),
            AccountMeta::new_readonly(reserve.lending_market, false),
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ];

        let mut data = vec![20u8]; // FlashRepayReserveLiquidity
        data.extend_from_slice(&amount.to_le_bytes());
        data.push(borrow_ix_index);

        Instruction {
            program_id: reserve.program_id,
            accounts,
            data,
        }
    }

    fn build_tip_instruction(&self, tip_lamports: u64) -> Instruction {
        let tip_account = random_tip_account();
        system_instruction::transfer(&self.payer.pubkey(), &tip_account, tip_lamports)
    }

    /// Calculate competitive tip based on market conditions
    fn calculate_tip(&self, expected_profit: u64) -> u64 {
        const MAX_TIP: u64 = 10_000_000_000; // 10 SOL
        const MIN_KEEPER_LAMPORTS: u64 = 5_000;

        let (p75, p95) = if let Some(cache) = &self.tip_floor_cache {
            (cache.get_p75(), cache.get_p95())
        } else {
            (10_000, 100_000)
        };

        let market_tip = if expected_profit > 1_000_000_000 {
            (p95 as f64 * 1.1) as u64
        } else if expected_profit > 100_000_000 {
            p75.saturating_mul(2)
        } else {
            (p75 as f64 * 1.3) as u64
        };

        let max_tip_from_profit = expected_profit / 2;
        let max_tip_after_keeper = expected_profit.saturating_sub(MIN_KEEPER_LAMPORTS);
        let effective_max = max_tip_from_profit.min(max_tip_after_keeper).min(MAX_TIP);

        let tip = market_tip.min(effective_max);
        tip.max(self.config.base_tip_lamports).max(1_000)
    }
}

/// Derive ATA address
/// Note: This uses TOKEN_PROGRAM for derivation - works for both SPL and Token-2022
/// because the ATA program uses the same derivation regardless of token program
fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM.as_ref(), mint.as_ref()],
        &ATA_PROGRAM,
    )
    .0
}

/// Derive ATA address for Token-2022 mints
#[allow(dead_code)]
fn derive_ata_token_2022(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_2022_PROGRAM.as_ref(), mint.as_ref()],
        &ATA_PROGRAM,
    )
    .0
}

/// Create ATA idempotent instruction
fn create_ata_idempotent(
    payer: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    // Derive ATA using the correct token program
    let ata = if *token_program == TOKEN_2022_PROGRAM {
        derive_ata_token_2022(owner, mint)
    } else {
        derive_ata(owner, mint)
    };
    
    let accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(ata, false),
        AccountMeta::new_readonly(*owner, false),
        AccountMeta::new_readonly(*mint, false),
        AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        AccountMeta::new_readonly(*token_program, false),
    ];

    Instruction {
        program_id: ATA_PROGRAM,
        accounts,
        data: vec![1u8], // create_idempotent
    }
}

/// Build SyncNative instruction for wSOL accounts
/// This syncs the token balance with the lamport balance after receiving native SOL
fn build_sync_native(ata: &Pubkey) -> Instruction {
    // SyncNative instruction (opcode 17)
    Instruction {
        program_id: TOKEN_PROGRAM,
        accounts: vec![AccountMeta::new(*ata, false)],
        data: vec![17u8], // SyncNative
    }
}

/// Build CloseAccount instruction to close a token account
/// Returns lamports to the destination and closes the account
fn build_close_account(account: &Pubkey, destination: &Pubkey, authority: &Pubkey) -> Instruction {
    // CloseAccount instruction (opcode 9)
    Instruction {
        program_id: TOKEN_PROGRAM,
        accounts: vec![
            AccountMeta::new(*account, false),      // Account to close
            AccountMeta::new(*destination, false),  // Destination for rent lamports
            AccountMeta::new_readonly(*authority, true), // Owner/authority
        ],
        data: vec![9u8], // CloseAccount
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::WSOL_MINT;

    #[test]
    fn test_ata_derivation() {
        let owner = Pubkey::new_unique();
        let mint = WSOL_MINT;
        let ata = derive_ata(&owner, &mint);
        assert_ne!(ata, owner);
        assert_ne!(ata, mint);
    }

    #[test]
    fn test_tip_calculation() {
        let config = BundleBuilderConfig::default();
        let payer = Arc::new(Keypair::new());
        let builder = ArbBundleBuilder::new(config.clone(), payer, HashMap::default(), Vec::new());

        assert_eq!(builder.calculate_tip(0), config.base_tip_lamports);

        let small_profit = 100_000;
        let tip_small = builder.calculate_tip(small_profit);
        assert!(tip_small >= 1_000);
        assert!(tip_small <= small_profit / 2);

        let large_profit = 2_000_000_000;
        let tip_large = builder.calculate_tip(large_profit);
        assert!(tip_large <= large_profit / 2);
        assert!(tip_large >= 1_000);
    }
}
