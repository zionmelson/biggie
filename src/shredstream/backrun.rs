//! Backrun Bundle Builder
//!
//! Constructs MEV bundles for backrunning victim transactions.
//! Bundle format: [victim_tx, arb_tx]
//!
//! The victim transaction executes first, moving the pool price.
//! Our arb transaction executes immediately after, capturing the price delta.

use super::evaluator::{ArbLeg, MevOpportunity};
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

/// A complete backrun bundle ready for submission
#[derive(Debug, Clone)]
pub struct BackrunBundle {
    /// Unique bundle ID
    pub id: String,
    /// The victim transaction (executed first)
    pub victim_tx: VersionedTransaction,
    /// Our arbitrage transaction (executed second)
    pub arb_tx: VersionedTransaction,
    /// Expected profit in lamports
    pub expected_profit_lamports: u64,
    /// Tip amount included
    pub tip_lamports: u64,
    /// The opportunity this bundle was built from
    pub opportunity_id: String,
}

impl BackrunBundle {
    /// Get both transactions as a vector for Jito submission
    pub fn transactions(&self) -> Vec<VersionedTransaction> {
        vec![self.victim_tx.clone(), self.arb_tx.clone()]
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
            compute_unit_price: 1000,   // 1000 micro-lamports
            base_tip_lamports: 25_000,  // Higher base for minimum competitiveness
            tip_profit_share_bps: 7000, // 70% - aggressive bidding wins auctions
            max_tip_profit_bps: 9000,   // 90% max - keep 10% minimum profit
            slippage_bps: 200,          // 2% - tighter for more accurate profit
            tip_floor_multiplier: 3.0,  // 3x P75 floor - outbid competitors
        }
    }
}

/// Bundle builder for MEV backruns
pub struct BackrunBundleBuilder {
    config: BundleBuilderConfig,
    payer: Arc<Keypair>,
    flash_loan_reserves: HashMap<Pubkey, FlashLoanReserve>,
    lookup_tables: Vec<AddressLookupTableAccount>,
    tip_floor_cache: Option<Arc<TipFloorCache>>,
}

impl BackrunBundleBuilder {
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

    /// Build a backrun bundle from an MEV opportunity
    pub fn build(&self, opportunity: &MevOpportunity, blockhash: Hash) -> Result<BackrunBundle> {
        // Validate victim transaction
        self.validate_victim_tx(&opportunity.victim_tx)?;

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

        Ok(BackrunBundle {
            id: format!("br_{}", opportunity.id),
            victim_tx: opportunity.victim_tx.clone(),
            arb_tx,
            expected_profit_lamports: opportunity.expected_profit_lamports,
            tip_lamports: tip,
            opportunity_id: opportunity.id.clone(),
        })
    }

    /// Validate the victim transaction before including in bundle
    fn validate_victim_tx(&self, tx: &VersionedTransaction) -> Result<()> {
        // Check that transaction has at least one signature
        if tx.signatures.is_empty() {
            bail!("Victim transaction has no signatures");
        }

        // Verify the first signature is not all zeros (invalid)
        let first_sig = &tx.signatures[0];
        if first_sig.as_ref().iter().all(|&b| b == 0) {
            bail!("Victim transaction has invalid signature (all zeros)");
        }

        // Check transaction has a valid message with instructions
        if tx.message.instructions().is_empty() {
            bail!("Victim transaction has no instructions");
        }

        // Verify blockhash is not default (would indicate a problem)
        let recent_blockhash = tx.message.recent_blockhash();
        if *recent_blockhash == Hash::default() {
            bail!("Victim transaction has default blockhash");
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
        opportunity: &MevOpportunity,
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

        // 5. Swap leg 2: intermediate -> borrow_mint
        let leg2_input = opportunity.arb_route.leg1.estimated_output;
        let swap2 = self.build_swap_instruction(&opportunity.arb_route.leg2, leg2_input)?;
        instructions.push(swap2);

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

    /// Calculate dynamic tip based on expected profit
    ///
    /// Uses tip floor data from Jito if available, otherwise falls back
    /// Profit-tiered tipping strategy for maximum competitiveness.
    ///
    /// Key insight: In NY with good infra, you'll tie other top searchers on latency.
    /// The bundle auction is won by TIP, so we tip aggressively based on profit tier.
    ///
    /// Strategy:
    /// - Small profits (<0.01 SOL): Tip 85% - these are competitive, guarantee the win
    /// - Medium profits (0.01-0.1 SOL): Tip 75% - still aggressive
    /// - Large profits (>0.1 SOL): Tip 65% - keep more, still competitive
    ///
    /// Always use MAX(floor-based, profit-share) to handle different market conditions.
    fn calculate_tip(&self, expected_profit: u64) -> u64 {
        // Sanity cap: max tip is 10 SOL even for huge profits
        const MAX_TIP: u64 = 10_000_000_000; // 10 SOL

        // Profit-tiered tip percentage (in bps)
        // Small profits: tip more aggressively to guarantee win
        // Large profits: can afford to keep more, still competitive
        let tier_bps = if expected_profit < 10_000_000 {
            // < 0.01 SOL: small profit, tip 85%
            8500
        } else if expected_profit < 100_000_000 {
            // 0.01 - 0.1 SOL: medium profit, tip 75%
            7500
        } else {
            // > 0.1 SOL: large profit, tip 65%
            6500
        };

        // Calculate profit-share tip
        let profit_share_tip = (expected_profit as u128 * tier_bps as u128 / 10000) as u64;

        // Calculate floor-based tip (if available)
        let floor_tip = if let Some(cache) = &self.tip_floor_cache {
            let p75 = cache.get_p75();
            let p95 = cache.get_p95();
            if p75 > 0 {
                // Use P95 for small profits (guarantee win), P75*3 for larger
                if expected_profit < 10_000_000 {
                    // Small profit: use P95 * 2 to dominate
                    p95.saturating_mul(2)
                } else {
                    // Larger profit: P75 * multiplier
                    ((p75 as f64) * self.config.tip_floor_multiplier) as u64
                }
            } else {
                0
            }
        } else {
            0
        };

        // Use MAX of floor-based and profit-share
        // This ensures competitiveness in both high-activity and low-activity periods
        let tip = profit_share_tip.max(floor_tip);

        // Cap at max_tip_profit_bps of profit and absolute MAX_TIP
        let max_tip_pct =
            (expected_profit as u128 * self.config.max_tip_profit_bps as u128 / 10000) as u64;

        tip.min(max_tip_pct)
            .min(MAX_TIP)
            .max(self.config.base_tip_lamports)
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
        let builder = BackrunBundleBuilder::new(config.clone(), payer, HashMap::new(), Vec::new());

        // Test minimum tip (base_tip_lamports is returned for 0 profit)
        assert_eq!(builder.calculate_tip(0), config.base_tip_lamports);

        // Test small profit tier (< 0.01 SOL = 10_000_000 lamports): 85% tip
        let small_profit = 100_000;
        let expected_small = small_profit * 8500 / 10000; // 85%
        assert_eq!(builder.calculate_tip(small_profit), expected_small);

        // Test medium profit tier (0.01-0.1 SOL): 75% tip
        let medium_profit = 50_000_000; // 0.05 SOL
        let expected_medium = medium_profit * 7500 / 10000; // 75%
        assert_eq!(builder.calculate_tip(medium_profit), expected_medium);

        // Test large profit tier (> 0.1 SOL): 65% tip
        let large_profit = 200_000_000; // 0.2 SOL
        let expected_large = large_profit * 6500 / 10000; // 65%
        assert_eq!(builder.calculate_tip(large_profit), expected_large);
    }
}
