//! Backrun Bundle Builder
//!
//! Constructs MEV bundles for backrunning victim transactions.
//! Bundle format: [victim_tx, arb_tx]
//!
//! The victim transaction executes first, moving the pool price.
//! Our arb transaction executes immediately after, capturing the price delta.

use super::evaluator::{ArbLeg, ArbRoute, MevOpportunity};
use super::programs::DexType;
use crate::executor::{fees, FlashLoanReserve, SwapAccount, SwapParams};
use crate::jito::random_tip_account;
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

/// Native SOL mint
pub const WSOL_MINT: Pubkey = solana_sdk::pubkey!("So11111111111111111111111111111111111111112");

/// Solend program ID
pub const SOLEND_PROGRAM: Pubkey =
    solana_sdk::pubkey!("So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo");

/// Token program
pub const TOKEN_PROGRAM: Pubkey =
    solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

/// Associated Token program
pub const ATA_PROGRAM: Pubkey =
    solana_sdk::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

/// System program
pub const SYSTEM_PROGRAM: Pubkey = solana_sdk::pubkey!("11111111111111111111111111111111");

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
}

impl Default for BundleBuilderConfig {
    fn default() -> Self {
        Self {
            compute_units: 400_000,
            compute_unit_price: 1000, // 1000 micro-lamports
            base_tip_lamports: 10_000,
            tip_profit_share_bps: 5000, // 50%
            max_tip_profit_bps: 8000,   // 80% max
            slippage_bps: 100,          // 1%
        }
    }
}

/// Bundle builder for MEV backruns
pub struct BackrunBundleBuilder {
    config: BundleBuilderConfig,
    payer: Arc<Keypair>,
    flash_loan_reserves: HashMap<Pubkey, FlashLoanReserve>,
    lookup_tables: Vec<AddressLookupTableAccount>,
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
        }
    }

    /// Build a backrun bundle from an MEV opportunity
    pub fn build(
        &self,
        opportunity: &MevOpportunity,
        blockhash: Hash,
    ) -> Result<BackrunBundle> {
        // Get flash loan reserve for the borrow mint
        let reserve = self
            .flash_loan_reserves
            .get(&opportunity.borrow_mint)
            .ok_or_else(|| anyhow!("No flash loan reserve for {}", opportunity.borrow_mint))?;

        let payer_pubkey = self.payer.pubkey();
        let borrow_ata = derive_ata(&payer_pubkey, &opportunity.borrow_mint);

        // Build the arb transaction
        let arb_tx = self.build_arb_transaction(
            opportunity,
            reserve,
            borrow_ata,
            blockhash,
        )?;

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

    /// Build the arbitrage transaction
    fn build_arb_transaction(
        &self,
        opportunity: &MevOpportunity,
        reserve: &FlashLoanReserve,
        borrow_ata: Pubkey,
        blockhash: Hash,
    ) -> Result<VersionedTransaction> {
        let payer_pubkey = self.payer.pubkey();
        let mut instructions = Vec::new();

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
        let swap1 = self.build_swap_instruction(
            &opportunity.arb_route.leg1,
            opportunity.borrow_amount,
        )?;
        instructions.push(swap1);

        // 5. Swap leg 2: intermediate -> borrow_mint
        let leg2_input = opportunity.arb_route.leg1.estimated_output;
        let swap2 = self.build_swap_instruction(
            &opportunity.arb_route.leg2,
            leg2_input,
        )?;
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
        let message = v0::Message::try_compile(
            &payer_pubkey,
            &instructions,
            &self.lookup_tables,
            blockhash,
        )?;

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

    // Placeholder swap builders - these need proper account derivation
    // In production, you'd look up pool state to get vaults, tick arrays, etc.

    fn build_raydium_amm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        // Raydium AMM V4 swap instruction
        // TODO: Look up pool state to get proper accounts
        bail!("Raydium AMM swap building not yet implemented for pool {}", leg.pool)
    }

    fn build_raydium_clmm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        // Raydium CLMM swap instruction
        // TODO: Derive tick arrays, vaults from pool state
        bail!("Raydium CLMM swap building not yet implemented for pool {}", leg.pool)
    }

    fn build_orca_whirlpool_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        // Orca Whirlpool swap instruction
        // TODO: Derive tick arrays, vaults, oracle from pool state
        bail!("Orca Whirlpool swap building not yet implemented for pool {}", leg.pool)
    }

    fn build_meteora_dlmm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        // Meteora DLMM swap instruction
        // TODO: Derive bin arrays from pool state
        bail!("Meteora DLMM swap building not yet implemented for pool {}", leg.pool)
    }

    fn build_meteora_damm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        // Meteora DAMM v2 swap instruction
        // TODO: Derive vaults from pool state
        bail!("Meteora DAMM swap building not yet implemented for pool {}", leg.pool)
    }

    fn build_pancake_clmm_swap(
        &self,
        leg: &ArbLeg,
        amount_in: u64,
        min_out: u64,
        input_ata: Pubkey,
        output_ata: Pubkey,
    ) -> Result<Instruction> {
        // PancakeSwap CLMM swap instruction
        // TODO: Derive tick arrays from pool state
        bail!("PancakeSwap CLMM swap building not yet implemented for pool {}", leg.pool)
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
    fn calculate_tip(&self, expected_profit: u64) -> u64 {
        // Profit-proportional tip, capped at max percentage
        let proportional_tip =
            expected_profit * self.config.tip_profit_share_bps / 10000;
        let max_tip = expected_profit * self.config.max_tip_profit_bps / 10000;

        proportional_tip
            .min(max_tip)
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
        let builder = BackrunBundleBuilder::new(
            config.clone(),
            payer,
            HashMap::new(),
            Vec::new(),
        );

        // Test minimum tip
        assert_eq!(builder.calculate_tip(0), config.base_tip_lamports);

        // Test proportional tip (50% of profit)
        let profit = 100_000;
        let expected = profit * 5000 / 10000; // 50%
        assert_eq!(builder.calculate_tip(profit), expected);
    }
}
