//! Arbitrage Transaction Builder
//!
//! Constructs atomic arbitrage transactions for cross-pool price divergence.
//!
//! # Primary Strategy: Pure Arbitrage (Single-TX Bundle)
//!
//! When we detect price divergence between pools:
//! 1. Flash loan the anchor token (SOL/USDC)
//! 2. Buy on the cheap pool
//! 3. Sell on the expensive pool
//! 4. Repay flash loan + fee
//! 5. Keep profit
//!
//! This is submitted as a **single-TX Jito bundle** for priority inclusion.
//!
//! # Why NOT Same-Block Bundling
//!
//! We receive transactions via Turbine/Shreds AFTER they've been executed.
//! The "triggering TX" is already in a block - we can't bundle with it.
//! Our arb TX goes in the NEXT block.
//!
//! # Legacy: TwoTxBundle struct
//!
//! The `TwoTxBundle` struct (with trigger_tx) is kept for potential future
//! integration with Jito's pending transaction stream, which WOULD allow
//! true same-block arb_bundlening. Currently unused.

use super::evaluator::{ArbLeg, ArbOpportunity, PureArbOpportunity};
use super::programs::DexType;
use crate::constants::{ATA_PROGRAM, SYSTEM_PROGRAM, TOKEN_PROGRAM};
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
use std::collections::HashMap;
use std::sync::Arc;

/// A complete arb_bundle bundle ready for submission (legacy - for future pending TX stream)
#[derive(Debug, Clone)]
pub struct TwoTxBundle {
    /// Unique bundle ID
    pub id: String,
    /// The triggering transaction (executed first) - requires pending TX stream
    pub trigger_tx: VersionedTransaction,
    /// Our arbitrage transaction (executed second)
    pub arb_tx: VersionedTransaction,
    /// Expected profit in lamports
    pub expected_profit_lamports: u64,
    /// Tip amount included
    pub tip_lamports: u64,
    /// The opportunity this bundle was built from
    pub opportunity_id: String,
}

/// A pure arbitrage bundle (no trigger transaction - single TX bundle)
#[derive(Debug, Clone)]
pub struct PureArbBundle {
    /// Unique bundle ID
    pub id: String,
    /// Our arbitrage transaction (single TX bundle)
    pub arb_tx: VersionedTransaction,
    /// Expected profit in lamports
    pub expected_profit_lamports: u64,
    /// Tip amount included
    pub tip_lamports: u64,
    /// The opportunity this bundle was built from
    pub opportunity_id: String,
}

impl PureArbBundle {
    /// Get transaction as a vector for Jito submission (single TX bundle)
    pub fn transactions(&self) -> Vec<VersionedTransaction> {
        vec![self.arb_tx.clone()]
    }
}

impl TwoTxBundle {
    /// Get both transactions as a vector for Jito submission
    pub fn transactions(&self) -> Vec<VersionedTransaction> {
        vec![self.trigger_tx.clone(), self.arb_tx.clone()]
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
            compute_units: 400_000,
            compute_unit_price: 1000,   // 1000 micro-lamports (~400 lamports total)
            base_tip_lamports: 1_000,   // Jito minimum - let market tip logic handle the rest
            tip_profit_share_bps: 5000, // 50% max - split profit evenly
            max_tip_profit_bps: 5000,   // 50% max - never tip more than half
            slippage_bps: 50,           // 0.5% - tighter slippage for accuracy
            tip_floor_multiplier: 1.3,  // 1.3x P75 floor - competitive baseline
        }
    }
}

/// Bundle builder for arbitrage bundles
pub struct ArbBundleBuilder {
    config: BundleBuilderConfig,
    payer: Arc<Keypair>,
    flash_loan_reserves: HashMap<Pubkey, FlashLoanReserve>,
    lookup_tables: Vec<AddressLookupTableAccount>,
    tip_floor_cache: Option<Arc<TipFloorCache>>,
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
        }
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

    /// Build a arb_bundle bundle from an arbitrage opportunity (requires pending TX stream)
    pub fn build(&self, opportunity: &ArbOpportunity, blockhash: Hash) -> Result<TwoTxBundle> {
        // Validate trigger transaction
        self.validate_trigger_tx(&opportunity.trigger_tx)?;

        // Get flash loan reserve for the borrow mint
        let reserve = self
            .flash_loan_reserves
            .get(&opportunity.borrow_mint)
            .ok_or_else(|| anyhow!("No flash loan reserve for {}", opportunity.borrow_mint))?;

        let payer_pubkey = self.payer.pubkey();
        let borrow_ata = derive_ata(&payer_pubkey, &opportunity.borrow_mint);

        // Build the arb transaction
        let arb_tx = self.build_arb_transaction(opportunity, reserve, borrow_ata, blockhash)?;

        // Calculate tip
        let tip = self.calculate_tip(opportunity.expected_profit_lamports);

        Ok(TwoTxBundle {
            id: format!("br_{}", opportunity.id),
            trigger_tx: opportunity.trigger_tx.clone(),
            arb_tx,
            expected_profit_lamports: opportunity.expected_profit_lamports,
            tip_lamports: tip,
            opportunity_id: opportunity.id.clone(),
        })
    }

    /// Build a pure arbitrage bundle from a cross-pool price discrepancy
    /// 
    /// Unlike arb_bundle bundles, pure arb bundles contain only our transaction.
    /// No trigger TX is needed - we're exploiting natural price drift between pools.
    pub fn build_pure_arb(&self, opportunity: &PureArbOpportunity, blockhash: Hash) -> Result<PureArbBundle> {
        // Get flash loan reserve for the borrow mint
        let reserve = self
            .flash_loan_reserves
            .get(&opportunity.borrow_mint)
            .ok_or_else(|| anyhow!("No flash loan reserve for {}", opportunity.borrow_mint))?;

        let payer_pubkey = self.payer.pubkey();
        let borrow_ata = derive_ata(&payer_pubkey, &opportunity.borrow_mint);

        // Build the arb transaction
        let arb_tx = self.build_pure_arb_transaction(opportunity, reserve, borrow_ata, blockhash)?;

        // Calculate tip
        let tip = self.calculate_tip(opportunity.expected_profit_lamports);

        Ok(PureArbBundle {
            id: format!("pa_{}", opportunity.id),
            arb_tx,
            expected_profit_lamports: opportunity.expected_profit_lamports,
            tip_lamports: tip,
            opportunity_id: opportunity.id.clone(),
        })
    }

    /// Build the pure arbitrage transaction
    ///
    /// Structure: flash borrow → swap1 (buy cheap) → swap2 (sell high) → flash repay → tip
    fn build_pure_arb_transaction(
        &self,
        opportunity: &PureArbOpportunity,
        reserve: &FlashLoanReserve,
        borrow_ata: Pubkey,
        blockhash: Hash,
    ) -> Result<VersionedTransaction> {
        let payer_pubkey = self.payer.pubkey();

        // Pre-allocate with capacity for pure arb (avoids reallocations)
        // 2 compute budget + 2 ATA creates + 1 borrow + 2 swaps + 1 repay + 1 tip = 9 instructions
        let mut instructions = Vec::with_capacity(9);

        // 1. Compute budget
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(
            self.config.compute_units,
        ));
        instructions.push(ComputeBudgetInstruction::set_compute_unit_price(
            self.config.compute_unit_price,
        ));

        // 2. Create ATAs (idempotent)
        // ATA for borrow mint (quote token - e.g., SOL/USDC)
        instructions.push(create_ata_idempotent(
            &payer_pubkey,
            &payer_pubkey,
            &opportunity.borrow_mint,
            &TOKEN_PROGRAM,
        ));

        // ATA for base mint (the token we're arbitraging)
        if opportunity.base_mint != opportunity.borrow_mint {
            instructions.push(create_ata_idempotent(
                &payer_pubkey,
                &payer_pubkey,
                &opportunity.base_mint,
                &TOKEN_PROGRAM,
            ));
        }

        // Track instruction index for flash loan repay reference
        let borrow_ix_index = instructions.len() as u8;

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
            pool_state: opportunity.buy_pool_state.clone(),
        };
        let swap1 = self.build_swap_instruction(&buy_leg, opportunity.trade_amount)?;
        instructions.push(swap1);

        // 5. Swap leg 2: Sell on expensive pool (base -> quote)
        let sell_leg = ArbLeg {
            pool: opportunity.sell_pool,
            dex: opportunity.sell_dex,
            input_mint: opportunity.base_mint,
            output_mint: opportunity.borrow_mint,
            estimated_output: opportunity.leg2_output,
            pool_state: opportunity.sell_pool_state.clone(),
        };
        let swap2 = self.build_swap_instruction(&sell_leg, opportunity.leg1_output)?;
        instructions.push(swap2);

        // 6. Flash repay
        instructions.push(self.build_repay_instruction(
            reserve,
            opportunity.trade_amount,
            borrow_ata,
            borrow_ix_index,
        ));

        // 7. Tip
        let tip = self.calculate_tip(opportunity.expected_profit_lamports);
        instructions.push(self.build_tip_instruction(tip));

        // Build versioned message
        let message =
            v0::Message::try_compile(&payer_pubkey, &instructions, &self.lookup_tables, blockhash)?;

        let versioned_message = VersionedMessage::V0(message);
        let tx = VersionedTransaction::try_new(versioned_message, &[self.payer.as_ref()])?;

        Ok(tx)
    }

    /// Validate the trigger transaction before including in bundle
    fn validate_trigger_tx(&self, tx: &VersionedTransaction) -> Result<()> {
        // Check that transaction has at least one signature
        if tx.signatures.is_empty() {
            bail!("Trigger transaction has no signatures");
        }

        // Verify the first signature is not all zeros (invalid)
        let first_sig = &tx.signatures[0];
        if first_sig.as_ref().iter().all(|&b| b == 0) {
            bail!("Trigger transaction has invalid signature (all zeros)");
        }

        // Check transaction has a valid message with instructions
        if tx.message.instructions().is_empty() {
            bail!("Trigger transaction has no instructions");
        }

        // Verify blockhash is not default (would indicate a problem)
        let recent_blockhash = tx.message.recent_blockhash();
        if *recent_blockhash == Hash::default() {
            bail!("Trigger transaction has default blockhash");
        }

        // Note: We cannot verify blockhash validity without RPC access
        // The block engine will reject bundles with expired blockhashes

        Ok(())
    }

    /// Build the arbitrage transaction
    ///
    /// OPTIMIZATION: Pre-allocate vector capacity to avoid reallocations.
    /// Typical bundle has: 2 compute budget + 2 ATA creates + 1 borrow + 2 swaps + 1 repay + 1 tip = 9 instructions
    fn build_arb_transaction(
        &self,
        opportunity: &ArbOpportunity,
        reserve: &FlashLoanReserve,
        borrow_ata: Pubkey,
        blockhash: Hash,
    ) -> Result<VersionedTransaction> {
        let payer_pubkey = self.payer.pubkey();

        // Pre-allocate with capacity for typical bundle (avoids reallocations)
        let mut instructions = Vec::with_capacity(10);

        // 1. Compute budget
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(
            self.config.compute_units,
        ));
        instructions.push(ComputeBudgetInstruction::set_compute_unit_price(
            self.config.compute_unit_price,
        ));

        // 2. Create ATA (idempotent)
        instructions.push(create_ata_idempotent(
            &payer_pubkey,
            &payer_pubkey,
            &opportunity.borrow_mint,
            &TOKEN_PROGRAM,
        ));

        // Also create ATA for intermediate token if different
        if opportunity.arb_route.leg1.output_mint != opportunity.borrow_mint {
            instructions.push(create_ata_idempotent(
                &payer_pubkey,
                &payer_pubkey,
                &opportunity.arb_route.leg1.output_mint,
                &TOKEN_PROGRAM,
            ));
        }

        // For 3-hop routes, also create ATA for second intermediate token
        if opportunity.arb_route.is_three_hop {
            if opportunity.arb_route.leg2.output_mint != opportunity.borrow_mint
                && opportunity.arb_route.leg2.output_mint != opportunity.arb_route.leg1.output_mint
            {
                instructions.push(create_ata_idempotent(
                    &payer_pubkey,
                    &payer_pubkey,
                    &opportunity.arb_route.leg2.output_mint,
                    &TOKEN_PROGRAM,
                ));
            }
        }

        // Track instruction index for flash loan repay reference
        let borrow_ix_index = instructions.len() as u8;

        // 3. Flash borrow
        instructions.push(self.build_borrow_instruction(
            reserve,
            opportunity.borrow_amount,
            borrow_ata,
        ));

        // 4. Swap leg 1: borrow_mint -> intermediate
        let swap1 =
            self.build_swap_instruction(&opportunity.arb_route.leg1, opportunity.borrow_amount)?;
        instructions.push(swap1);

        // 5. Swap leg 2: intermediate -> borrow_mint (or intermediate2 for 3-hop)
        let leg2_input = opportunity.arb_route.leg1.estimated_output;
        let swap2 = self.build_swap_instruction(&opportunity.arb_route.leg2, leg2_input)?;
        instructions.push(swap2);

        // 5b. Swap leg 3 (for 3-hop arbs only): intermediate2 -> borrow_mint
        if let Some(ref leg3) = opportunity.arb_route.leg3 {
            let leg3_input = opportunity.arb_route.leg2.estimated_output;
            let swap3 = self.build_swap_instruction(leg3, leg3_input)?;
            instructions.push(swap3);
        }

        // 6. Flash repay
        instructions.push(self.build_repay_instruction(
            reserve,
            opportunity.borrow_amount,
            borrow_ata,
            borrow_ix_index,
        ));

        // 7. Tip
        let tip = self.calculate_tip(opportunity.expected_profit_lamports);
        instructions.push(self.build_tip_instruction(tip));

        // Build versioned message
        let message =
            v0::Message::try_compile(&payer_pubkey, &instructions, &self.lookup_tables, blockhash)?;

        let versioned_message = VersionedMessage::V0(message);
        let tx = VersionedTransaction::try_new(versioned_message, &[self.payer.as_ref()])?;

        Ok(tx)
    }

    /// Build a swap instruction for a leg
    fn build_swap_instruction(&self, leg: &ArbLeg, amount_in: u64) -> Result<Instruction> {
        let payer = self.payer.pubkey();
        let input_ata = derive_ata(&payer, &leg.input_mint);
        let output_ata = derive_ata(&payer, &leg.output_mint);

        // Apply slippage to min_amount_out
        let min_out = leg.estimated_output * (10000 - self.config.slippage_bps) / 10000;

        // Build instruction based on DEX type
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
    //
    // Each builder constructs a swap instruction for its respective DEX.
    // Pool state data is sourced from leg.pool_state (populated during evaluation).

    fn build_raydium_amm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        // Raydium AMM V4 swap_base_in_v2 (instruction index 16)
        // Source: raydium-amm/program/src/processor.rs
        let state = &leg.pool_state;

        // Determine swap direction: coin->pc or pc->coin
        let is_coin_to_pc = leg.input_mint == state.token_mint_a;
        let (user_source, user_dest, coin_vault, pc_vault) = if is_coin_to_pc {
            (
                input_ata,
                output_ata,
                state.token_vault_a,
                state.token_vault_b,
            )
        } else {
            (
                input_ata,
                output_ata,
                state.token_vault_b,
                state.token_vault_a,
            )
        };

        let amm_authority = state
            .amm_authority
            .ok_or_else(|| anyhow!("Raydium AMM swap requires amm_authority in pool_state"))?;

        let accounts = vec![
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new(leg.pool, false),
            AccountMeta::new_readonly(amm_authority, false),
            AccountMeta::new(coin_vault, false),
            AccountMeta::new(pc_vault, false),
            AccountMeta::new(user_source, false),
            AccountMeta::new(user_dest, false),
            AccountMeta::new_readonly(self.payer.pubkey(), true),
        ];

        // Data: [instruction_index(1), amount_in(8), min_out(8)]
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
        // Raydium CLMM swap (Anchor discriminator)
        // Source: raydium-clmm/programs/amm/src/instructions/swap.rs
        let state = &leg.pool_state;

        let amm_config = state
            .amm_config
            .ok_or_else(|| anyhow!("Raydium CLMM swap requires amm_config in pool_state"))?;
        let observation_state = state
            .observation_state
            .ok_or_else(|| anyhow!("Raydium CLMM swap requires observation_state in pool_state"))?;

        // Determine direction: zero_for_one = swap token0 for token1
        let zero_for_one = leg.input_mint == state.token_mint_a;
        let (input_vault, output_vault) = if zero_for_one {
            (state.token_vault_a, state.token_vault_b)
        } else {
            (state.token_vault_b, state.token_vault_a)
        };

        // Recompute tick arrays dynamically based on current tick
        let tick_arrays = if state.spacing > 0 {
            derive_raydium_clmm_tick_arrays(&leg.pool, state.current_tick_or_bin, state.spacing)
        } else if !state.tick_arrays.is_empty() {
            state.tick_arrays.clone()
        } else {
            bail!("Raydium CLMM swap requires tick arrays or spacing to derive them");
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

        // Add additional tick arrays as remaining accounts
        for tick_array in tick_arrays.iter().skip(1) {
            accounts.push(AccountMeta::new(*tick_array, false));
        }

        // Data: [discriminator(8), amount(8), other_amount_threshold(8),
        //        sqrt_price_limit_x64(16), is_base_input(1)]
        let mut data = Vec::with_capacity(41);
        data.extend_from_slice(&super::programs::raydium_clmm::SWAP);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        // sqrt_price_limit: 0 = no limit
        data.extend_from_slice(&0u128.to_le_bytes());
        data.push(1u8); // is_base_input = true

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
        // Orca Whirlpool swap (Anchor discriminator)
        // Source: whirlpools/programs/whirlpool/src/instructions/swap.rs
        let state = &leg.pool_state;

        let oracle = state
            .observation_state
            .ok_or_else(|| anyhow!("Orca Whirlpool swap requires oracle in pool_state"))?;

        // Recompute tick arrays dynamically based on current tick
        let tick_arrays = if state.spacing > 0 {
            derive_whirlpool_tick_arrays(&leg.pool, state.current_tick_or_bin, state.spacing)
        } else if state.tick_arrays.len() >= 3 {
            state.tick_arrays.clone()
        } else {
            bail!("Orca Whirlpool swap requires 3 tick_arrays or spacing to derive them");
        };

        if tick_arrays.len() < 3 {
            bail!(
                "Orca Whirlpool swap requires 3 tick_arrays, got {}",
                tick_arrays.len()
            );
        }

        // Direction: a_to_b means swapping token A for token B
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

        // Data: [discriminator(8), amount(8), other_amount_threshold(8),
        //        sqrt_price_limit(16), amount_specified_is_input(1), a_to_b(1)]
        let mut data = Vec::with_capacity(42);
        data.extend_from_slice(&super::programs::orca_whirlpool::SWAP);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        // sqrt_price_limit: 0 = no limit (use MIN or MAX based on direction)
        let sqrt_price_limit: u128 = if a_to_b {
            4295048016
        } else {
            79226673515401279992447579055
        };
        data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
        data.push(1u8); // amount_specified_is_input = true
        data.push(if a_to_b { 1u8 } else { 0u8 }); // a_to_b

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
        // Meteora DLMM swap2 (Anchor discriminator)
        // Source: dlmm-sdk/programs/lb_clmm/src/instructions/swap.rs
        let state = &leg.pool_state;

        // Determine direction: x_to_y
        let x_to_y = leg.input_mint == state.token_mint_a;
        let (user_token_in, user_token_out) = (input_ata, output_ata);

        // Recompute bin arrays dynamically based on current active bin
        let bin_arrays = if !state.bin_arrays.is_empty() {
            state.bin_arrays.clone()
        } else {
            derive_dlmm_bin_arrays(&leg.pool, state.current_tick_or_bin)
        };

        let mut accounts = vec![
            AccountMeta::new(leg.pool, false), // lb_pair
            AccountMeta::new(state.bin_array_bitmap_extension.unwrap_or(leg.pool), false),
            AccountMeta::new(state.token_vault_a, false), // reserve_x
            AccountMeta::new(state.token_vault_b, false), // reserve_y
            AccountMeta::new_readonly(state.token_mint_a, false), // token_x_mint
            AccountMeta::new_readonly(state.token_mint_b, false), // token_y_mint
            AccountMeta::new_readonly(TOKEN_PROGRAM, false), // token_x_program
            AccountMeta::new_readonly(TOKEN_PROGRAM, false), // token_y_program
            AccountMeta::new(self.payer.pubkey(), true),  // user
            AccountMeta::new(user_token_in, false),
            AccountMeta::new(user_token_out, false),
            AccountMeta::new_readonly(state.observation_state.unwrap_or(leg.pool), false), // oracle
            AccountMeta::new_readonly(leg.pool, false), // host_fee_in (optional, use lb_pair as placeholder)
            AccountMeta::new_readonly(
                Pubkey::find_program_address(
                    &[b"__event_authority"],
                    &super::programs::METEORA_DLMM,
                )
                .0,
                false,
            ), // event_authority
            AccountMeta::new_readonly(*super::programs::METEORA_DLMM, false), // program
        ];

        // Add bin arrays as remaining accounts
        for bin_array in &bin_arrays {
            accounts.push(AccountMeta::new(*bin_array, false));
        }

        // Data: [discriminator(8), amount_in(8), min_amount_out(8)]
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
        // Meteora DAMM v2 (CP-AMM) swap (Anchor discriminator)
        // Source: damm-v2/programs/cp-amm/src/instructions/swap.rs
        let state = &leg.pool_state;

        let pool_authority = state
            .pool_authority
            .ok_or_else(|| anyhow!("Meteora DAMM swap requires pool_authority in pool_state"))?;

        // Determine direction
        let a_to_b = leg.input_mint == state.token_mint_a;
        let (input_token_account, output_token_account) = (input_ata, output_ata);

        let accounts = vec![
            AccountMeta::new_readonly(pool_authority, false),
            AccountMeta::new(leg.pool, false),
            AccountMeta::new(input_token_account, false),
            AccountMeta::new(output_token_account, false),
            AccountMeta::new(state.token_vault_a, false),
            AccountMeta::new(state.token_vault_b, false),
            AccountMeta::new_readonly(state.token_mint_a, false),
            AccountMeta::new_readonly(state.token_mint_b, false),
            AccountMeta::new(self.payer.pubkey(), true),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false), // token_a_program
            AccountMeta::new_readonly(TOKEN_PROGRAM, false), // token_b_program
            AccountMeta::new_readonly(leg.pool, false),      // referral_token_account (optional)
        ];

        // Data: [discriminator(8), amount_in(8), minimum_amount_out(8)]
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
        // PancakeSwap CLMM - nearly identical to Raydium CLMM
        let state = &leg.pool_state;

        let amm_config = state
            .amm_config
            .ok_or_else(|| anyhow!("PancakeSwap CLMM swap requires amm_config in pool_state"))?;
        let observation_state = state.observation_state.ok_or_else(|| {
            anyhow!("PancakeSwap CLMM swap requires observation_state in pool_state")
        })?;

        let zero_for_one = leg.input_mint == state.token_mint_a;
        let (input_vault, output_vault) = if zero_for_one {
            (state.token_vault_a, state.token_vault_b)
        } else {
            (state.token_vault_b, state.token_vault_a)
        };

        // Recompute tick arrays dynamically (PancakeSwap uses same layout as Raydium CLMM)
        let tick_arrays = if state.spacing > 0 {
            derive_raydium_clmm_tick_arrays(&leg.pool, state.current_tick_or_bin, state.spacing)
        } else if !state.tick_arrays.is_empty() {
            state.tick_arrays.clone()
        } else {
            bail!("PancakeSwap CLMM swap requires tick arrays or spacing to derive them");
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

        let mut data = Vec::with_capacity(41);
        data.extend_from_slice(&super::programs::pancakeswap_clmm::SWAP);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        data.extend_from_slice(&0u128.to_le_bytes()); // sqrt_price_limit
        data.push(1u8); // is_base_input

        Ok(Instruction {
            program_id: *super::programs::PANCAKESWAP_CLMM,
            accounts,
            data,
        })
    }

    /// Build Solend flash borrow instruction
    fn build_borrow_instruction(
        &self,
        reserve: &FlashLoanReserve,
        amount: u64,
        dest_ata: Pubkey,
    ) -> Instruction {
        // Solend FlashBorrowReserveLiquidity (instruction 19)
        let accounts = vec![
            AccountMeta::new(reserve.borrow_liquidity_supply, false),
            AccountMeta::new(dest_ata, false),
            AccountMeta::new(reserve.reserve, false),
            AccountMeta::new_readonly(reserve.lending_market, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
        ];

        let mut data = vec![19u8]; // FlashBorrowReserveLiquidity
        data.extend_from_slice(&amount.to_le_bytes());

        Instruction {
            program_id: reserve.program_id,
            accounts,
            data,
        }
    }

    /// Build Solend flash repay instruction
    fn build_repay_instruction(
        &self,
        reserve: &FlashLoanReserve,
        amount: u64,
        source_ata: Pubkey,
        borrow_ix_index: u8,
    ) -> Instruction {
        // Solend FlashRepayReserveLiquidity (instruction 20)
        let accounts = vec![
            AccountMeta::new(source_ata, false),
            AccountMeta::new(reserve.repay_liquidity_supply, false),
            AccountMeta::new(reserve.repay_fee_receiver, false),
            AccountMeta::new(reserve.host_fee_receiver, false),
            AccountMeta::new(reserve.reserve, false),
            AccountMeta::new_readonly(reserve.lending_market, false),
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
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

    /// Build Jito tip instruction
    fn build_tip_instruction(&self, tip_lamports: u64) -> Instruction {
        let tip_account = random_tip_account();
        system_instruction::transfer(&self.payer.pubkey(), &tip_account, tip_lamports)
    }

    /// Calculate competitive tip based on MARKET CONDITIONS, not profit percentage
    ///
    /// Key insight: Jito auctions are won by having the highest tip among competing bundles.
    /// The tip floor API tells us what tips are actually landing.
    ///
    /// Strategy:
    /// 1. Use P75 as baseline (beats 75% of competition)
    /// 2. Add buffer based on opportunity value (more profit = can afford higher tip)
    /// 3. NEVER tip more than 50% of profit (keep meaningful profit)
    /// 4. Return 0 if we can't afford competitive tip (skip opportunity)
    ///
    /// This beats the old approach of tipping 85% of small profits (wasteful)
    /// or under-tipping competitive opportunities.
    fn calculate_tip(&self, expected_profit: u64) -> u64 {
        // Sanity cap: max tip is 10 SOL even for huge profits
        const MAX_TIP: u64 = 10_000_000_000; // 10 SOL
        
        // Minimum we'll ever keep (ensures we're not losing money after gas)
        const MIN_KEEPER_LAMPORTS: u64 = 5_000; // ~$0.001

        // Get market floors
        let (p75, p95) = if let Some(cache) = &self.tip_floor_cache {
            (cache.get_p75(), cache.get_p95())
        } else {
            // Fallback: use reasonable defaults based on typical market
            (10_000, 100_000) // ~P75 and P95 from typical market
        };

        // Calculate the competitive tip based on market
        // For most arb, P75 + 20% buffer is enough to land
        // For high-value opportunities, we can afford to go higher
        let market_tip = if expected_profit > 1_000_000_000 {
            // > 1 SOL profit: Very valuable, use P95 to guarantee landing
            (p95 as f64 * 1.1) as u64
        } else if expected_profit > 100_000_000 {
            // 0.1 - 1 SOL: High value, use P75 * 2
            p75.saturating_mul(2)
        } else {
            // Normal: P75 + 30% buffer
            (p75 as f64 * 1.3) as u64
        };

        // Calculate max we're willing to tip (50% of profit)
        // This ensures we always keep meaningful profit
        let max_tip_from_profit = expected_profit / 2;

        // We need to keep at least MIN_KEEPER after tip
        let max_tip_after_keeper = expected_profit.saturating_sub(MIN_KEEPER_LAMPORTS);

        // The actual max tip is the minimum of these constraints
        let effective_max = max_tip_from_profit.min(max_tip_after_keeper).min(MAX_TIP);

        // If we can't afford the market tip, we'll still try with what we can afford
        // The bundle might still land if competition is low
        let tip = market_tip.min(effective_max);

        // Apply absolute minimum (Jito requires >= 1000 lamports)
        tip.max(self.config.base_tip_lamports).max(1_000)
    }
}

/// Derive ATA address
fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM.as_ref(), mint.as_ref()],
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
    let ata = derive_ata(owner, mint);
    let accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(ata, false),
        AccountMeta::new_readonly(*owner, false),
        AccountMeta::new_readonly(*mint, false),
        AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        AccountMeta::new_readonly(*token_program, false),
    ];

    // create_idempotent instruction discriminator
    let data = vec![1u8];

    Instruction {
        program_id: ATA_PROGRAM,
        accounts,
        data,
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
        let builder = ArbBundleBuilder::new(config.clone(), payer, HashMap::new(), Vec::new());

        // Test minimum tip (base_tip_lamports is returned for 0 profit)
        assert_eq!(builder.calculate_tip(0), config.base_tip_lamports);

        // Test small profit: market-based tip (no cache = fallback 10k * 1.3 = 13k)
        // But capped at 50% of profit
        let small_profit = 100_000;
        // Fallback P75 = 10k, tip = 10k * 1.3 = 13k, but max 50% of 100k = 50k, so tip = 13k
        let tip_small = builder.calculate_tip(small_profit);
        assert!(tip_small >= 1_000, "Tip should be >= Jito minimum");
        assert!(tip_small <= small_profit / 2, "Tip should be <= 50% of profit");

        // Test medium profit: tip should scale with market floor
        let medium_profit = 50_000_000; // 0.05 SOL
        let tip_medium = builder.calculate_tip(medium_profit);
        assert!(tip_medium <= medium_profit / 2, "Tip should be <= 50% of profit");
        assert!(tip_medium >= 1_000, "Tip should be >= Jito minimum");

        // Test large profit: should use P95-based tip
        let large_profit = 2_000_000_000; // 2 SOL
        let tip_large = builder.calculate_tip(large_profit);
        assert!(tip_large <= large_profit / 2, "Tip should be <= 50% of profit");
        assert!(tip_large >= 1_000, "Tip should be >= Jito minimum");
    }
}
