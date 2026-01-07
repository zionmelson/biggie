//! Transaction execution
//!
//! Build arb transactions with flash loans, submit via Jito.

use crate::arb::ArbOpportunity;
use crate::config::{Config, FlashLoanConfig};
use crate::pools::{DexType, Pool, PoolRegistry};
use anyhow::{anyhow, Result};
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::str::FromStr;
use std::sync::Arc;

/// Known program IDs
pub mod programs {
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;
    use std::sync::LazyLock;

    pub fn token_program() -> Pubkey {
        Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap()
    }

    pub fn associated_token_program() -> Pubkey {
        Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap()
    }

    pub fn system_program() -> Pubkey {
        Pubkey::from_str("11111111111111111111111111111111").unwrap()
    }

    /// Raydium AMM V4 program ID
    pub static RAYDIUM_AMM: LazyLock<Pubkey> = LazyLock::new(|| {
        Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap()
    });

    pub fn raydium_amm() -> Pubkey {
        *RAYDIUM_AMM
    }

    /// Raydium AMM authority PDA (global, not per-pool)
    /// Derived with seed "amm authority"
    pub static RAYDIUM_AMM_AUTHORITY: LazyLock<Pubkey> = LazyLock::new(|| {
        Pubkey::find_program_address(&[b"amm authority"], &RAYDIUM_AMM).0
    });

    pub fn raydium_amm_authority() -> Pubkey {
        *RAYDIUM_AMM_AUTHORITY
    }

    pub fn raydium_clmm() -> Pubkey {
        Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").unwrap()
    }

    pub fn orca_whirlpool() -> Pubkey {
        Pubkey::from_str("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc").unwrap()
    }

    pub fn meteora_dlmm() -> Pubkey {
        Pubkey::from_str("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo").unwrap()
    }

    pub fn meteora_damm() -> Pubkey {
        Pubkey::from_str("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB").unwrap()
    }

    pub fn pancakeswap_clmm() -> Pubkey {
        Pubkey::from_str("HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq").unwrap()
    }

    pub fn memo_program() -> Pubkey {
        Pubkey::from_str("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr").unwrap()
    }

    /// Derive Orca Whirlpool oracle PDA
    pub fn whirlpool_oracle_pda(whirlpool: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[b"oracle", whirlpool.as_ref()], &orca_whirlpool()).0
    }

    /// Derive Orca Whirlpool tick array PDA
    /// NOTE: Orca uses the STRING representation of the tick index as seed!
    pub fn whirlpool_tick_array_pda(whirlpool: &Pubkey, start_tick_index: i32) -> Pubkey {
        let tick_str = start_tick_index.to_string();
        Pubkey::find_program_address(
            &[b"tick_array", whirlpool.as_ref(), tick_str.as_bytes()],
            &orca_whirlpool(),
        ).0
    }

    /// Derive PancakeSwap CLMM tick array PDA
    /// NOTE: PancakeSwap uses BIG ENDIAN bytes for the tick index!
    pub fn pancakeswap_tick_array_pda(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
        Pubkey::find_program_address(
            &[b"tick_array", pool.as_ref(), &start_tick_index.to_be_bytes()],
            &pancakeswap_clmm(),
        ).0
    }

    pub fn token_program_2022() -> Pubkey {
        Pubkey::from_str("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb").unwrap()
    }

    /// Derive Meteora DLMM oracle PDA
    pub fn dlmm_oracle_pda(lb_pair: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[b"oracle", lb_pair.as_ref()], &meteora_dlmm()).0
    }

    /// Derive Meteora DLMM bin array PDA
    pub fn dlmm_bin_array_pda(lb_pair: &Pubkey, bin_array_index: i64) -> Pubkey {
        Pubkey::find_program_address(
            &[b"bin_array", lb_pair.as_ref(), &bin_array_index.to_le_bytes()],
            &meteora_dlmm(),
        ).0
    }

    /// Derive Meteora DLMM event authority PDA
    pub fn dlmm_event_authority() -> Pubkey {
        Pubkey::find_program_address(&[b"__event_authority"], &meteora_dlmm()).0
    }

    pub fn solend() -> Pubkey {
        Pubkey::from_str("So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo").unwrap()
    }

    pub fn jito_tip() -> Pubkey {
        // One of the Jito tip accounts
        Pubkey::from_str("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5").unwrap()
    }
}

/// Build arbitrage transaction
pub struct TxBuilder {
    pub payer: Arc<Keypair>,
    pub flash_loan: FlashLoanConfig,
    pub compute_units: u32,
    pub compute_unit_price: u64,
    pub tip_bps: u16,
}

impl TxBuilder {
    pub fn new(config: &Config, payer: Arc<Keypair>) -> Self {
        Self {
            payer,
            flash_loan: config.flash_loan.clone(),
            compute_units: config.compute_units,
            compute_unit_price: config.compute_unit_price,
            tip_bps: config.tip_bps,
        }
    }

    /// Build complete arb transaction
    pub fn build_arb_tx(
        &self,
        opp: &ArbOpportunity,
        registry: &PoolRegistry,
        blockhash: Hash,
    ) -> Result<Transaction> {
        let buy_pool = registry.get_pool(&opp.buy_pool)
            .ok_or_else(|| anyhow!("Buy pool not found"))?;
        let sell_pool = registry.get_pool(&opp.sell_pool)
            .ok_or_else(|| anyhow!("Sell pool not found"))?;

        let mut instructions = Vec::new();

        // 1. Compute budget
        instructions.push(self.compute_budget_ix());
        instructions.push(self.compute_price_ix());

        // 2. Flash loan borrow
        instructions.push(self.flash_borrow_ix(opp.borrow_amount)?);

        // 3. Swap on buy pool (quote → base)
        instructions.push(self.swap_ix(
            buy_pool,
            opp.borrow_amount,
            opp.min_buy_output,
            !opp.buy_a_sell_a, // If buying A, we swap B→A
        )?);

        // 4. Swap on sell pool (base → quote)
        instructions.push(self.swap_ix(
            sell_pool,
            opp.buy_output, // Use expected output, slippage handled by min_amount
            opp.min_sell_output,
            opp.buy_a_sell_a, // If selling A, we swap A→B
        )?);

        // 5. Flash loan repay
        let repay_amount = opp.borrow_amount + crate::math::flash_loan_fee(opp.borrow_amount);
        instructions.push(self.flash_repay_ix(repay_amount)?);

        // 6. Jito tip (percentage of profit)
        let tip_amount = opp.expected_profit * self.tip_bps as u64 / 10000;
        if tip_amount > 0 {
            instructions.push(self.tip_ix(tip_amount));
        }

        // Build transaction
        let message = Message::new(&instructions, Some(&self.payer.pubkey()));
        let mut tx = Transaction::new_unsigned(message);
        tx.message.recent_blockhash = blockhash;
        tx.sign(&[&*self.payer], blockhash);

        Ok(tx)
    }

    fn compute_budget_ix(&self) -> Instruction {
        // SetComputeUnitLimit instruction
        let data = [
            2u8, // instruction index
            self.compute_units.to_le_bytes()[0],
            self.compute_units.to_le_bytes()[1],
            self.compute_units.to_le_bytes()[2],
            self.compute_units.to_le_bytes()[3],
        ];
        Instruction {
            program_id: Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap(),
            accounts: vec![],
            data: data.to_vec(),
        }
    }

    fn compute_price_ix(&self) -> Instruction {
        // SetComputeUnitPrice instruction
        let mut data = vec![3u8]; // instruction index
        data.extend_from_slice(&self.compute_unit_price.to_le_bytes());
        Instruction {
            program_id: Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap(),
            accounts: vec![],
            data,
        }
    }

    fn flash_borrow_ix(&self, amount: u64) -> Result<Instruction> {
        // Solend FlashBorrowReserveLiquidity
        // Instruction index: 20
        let mut data = vec![20u8];
        data.extend_from_slice(&amount.to_le_bytes());

        let accounts = vec![
            AccountMeta::new(self.flash_loan.reserve_liquidity_supply(), false),
            AccountMeta::new(self.payer_token_account(&self.flash_loan.liquidity_mint())?, false),
            AccountMeta::new(self.flash_loan.reserve(), false),
            AccountMeta::new_readonly(self.flash_loan.lending_market(), false),
            AccountMeta::new_readonly(programs::token_program(), false),
        ];

        Ok(Instruction {
            program_id: self.flash_loan.program_id(),
            accounts,
            data,
        })
    }

    fn flash_repay_ix(&self, amount: u64) -> Result<Instruction> {
        // Solend FlashRepayReserveLiquidity
        // Instruction index: 21
        let mut data = vec![21u8];
        data.extend_from_slice(&amount.to_le_bytes());
        // borrow_instruction_index (u8) - typically 2 (after compute budget ixs)
        data.push(2);

        let accounts = vec![
            AccountMeta::new(self.payer_token_account(&self.flash_loan.liquidity_mint())?, false),
            AccountMeta::new(self.flash_loan.reserve_liquidity_supply(), false),
            AccountMeta::new(self.flash_loan.reserve_liquidity_fee_receiver(), false),
            AccountMeta::new(self.flash_loan.reserve(), false),
            AccountMeta::new_readonly(self.flash_loan.lending_market(), false),
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new_readonly(programs::token_program(), false),
        ];

        Ok(Instruction {
            program_id: self.flash_loan.program_id(),
            accounts,
            data,
        })
    }

    fn swap_ix(
        &self,
        pool: &Pool,
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> Result<Instruction> {
        match pool.dex {
            DexType::RaydiumAmm => self.raydium_amm_swap_ix(pool, amount_in, min_amount_out, a_to_b),
            DexType::RaydiumClmm => self.raydium_clmm_swap_ix(pool, amount_in, min_amount_out, a_to_b),
            DexType::MeteoraDamm => self.meteora_damm_swap_ix(pool, amount_in, min_amount_out, a_to_b),
            DexType::OrcaWhirlpool => self.orca_whirlpool_swap_ix(pool, amount_in, min_amount_out, a_to_b),
            DexType::MeteoraDlmm => self.meteora_dlmm_swap_ix(pool, amount_in, min_amount_out, a_to_b),
            DexType::PancakeSwapClmm => self.pancakeswap_clmm_swap_ix(pool, amount_in, min_amount_out, a_to_b),
        }
    }

    fn raydium_amm_swap_ix(
        &self,
        pool: &Pool,
        amount_in: u64,
        min_amount_out: u64,
        _a_to_b: bool, // Not used - vaults passed in fixed order
    ) -> Result<Instruction> {
        // Raydium AMM SwapBaseInV2 instruction (opcode 16)
        // 
        // This is the simplified V2 swap that doesn't require Serum accounts.
        // Account order is FIXED regardless of swap direction:
        //   - coin_vault (vault_a) always first
        //   - pc_vault (vault_b) always second
        //   - user_source and user_dest depend on direction
        //
        // Accounts:
        // 0. [] Token program
        // 1. [writable] AMM Account (pool state)
        // 2. [] AMM authority (PDA: "amm authority")
        // 3. [writable] AMM coin vault (always vault_a)
        // 4. [writable] AMM pc vault (always vault_b)
        // 5. [writable] User source token account
        // 6. [writable] User destination token account
        // 7. [signer] User wallet

        let (source_mint, dest_mint) = if _a_to_b {
            (pool.mint_a, pool.mint_b)
        } else {
            (pool.mint_b, pool.mint_a)
        };

        let source_ata = self.payer_token_account(&source_mint)?;
        let dest_ata = self.payer_token_account(&dest_mint)?;

        let accounts = vec![
            AccountMeta::new_readonly(programs::token_program(), false),
            AccountMeta::new(pool.address, false),                    // AMM account
            AccountMeta::new_readonly(programs::raydium_amm_authority(), false), // AMM authority PDA
            AccountMeta::new(pool.vault_a, false),                    // coin_vault (always vault_a)
            AccountMeta::new(pool.vault_b, false),                    // pc_vault (always vault_b)
            AccountMeta::new(source_ata, false),                      // user source
            AccountMeta::new(dest_ata, false),                        // user dest
            AccountMeta::new_readonly(self.payer.pubkey(), true),     // user wallet
        ];

        // Instruction data: opcode (1 byte) + amount_in (8 bytes) + min_out (8 bytes)
        let mut data = Vec::with_capacity(17);
        data.push(16u8); // SwapBaseInV2 opcode
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        Ok(Instruction {
            program_id: programs::raydium_amm(),
            accounts,
            data,
        })
    }

    /// Raydium CLMM swap instruction
    ///
    /// Account order (verified from successful mainnet tx):
    /// 0: payer (signer)
    /// 1: amm_config
    /// 2: pool_state
    /// 3: input_token_account (user)
    /// 4: output_token_account (user)
    /// 5: input_vault
    /// 6: output_vault
    /// 7: observation_state
    /// 8: token_program
    /// 9+: tick_arrays (remaining accounts)
    fn raydium_clmm_swap_ix(
        &self,
        pool: &Pool,
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> Result<Instruction> {
        // Require CLMM-specific fields
        let amm_config = pool.amm_config
            .ok_or_else(|| anyhow!("Pool missing amm_config - run tick array discovery first"))?;
        let observation_state = pool.observation_state
            .ok_or_else(|| anyhow!("Pool missing observation_state"))?;
        let tick_spacing = pool.tick_spacing
            .ok_or_else(|| anyhow!("Pool missing tick_spacing"))?;
        let current_tick = pool.current_tick
            .ok_or_else(|| anyhow!("Pool missing current_tick"))?;
        
        if pool.tick_arrays.is_empty() {
            return Err(anyhow!("Pool has no tick arrays - run tick array discovery first"));
        }

        // Select tick arrays for this swap direction
        // CRITICAL: zero_for_one semantics:
        //   - zero_for_one = false: selling token_0 for token_1 (A→B), tick goes UP
        //   - zero_for_one = true: selling token_1 for token_0 (B→A), tick goes DOWN
        let zero_for_one = !a_to_b; // A→B = false, B→A = true
        let tick_arrays = crate::pools::select_tick_arrays_for_swap(
            &pool.tick_arrays,
            current_tick,
            tick_spacing,
            zero_for_one,
        );
        
        if tick_arrays.is_empty() {
            return Err(anyhow!("No tick arrays available for swap direction"));
        }

        // Determine vaults and user accounts based on direction
        let (input_vault, output_vault, source_mint, dest_mint) = if a_to_b {
            (pool.vault_a, pool.vault_b, pool.mint_a, pool.mint_b)
        } else {
            (pool.vault_b, pool.vault_a, pool.mint_b, pool.mint_a)
        };

        let user_input = self.payer_token_account(&source_mint)?;
        let user_output = self.payer_token_account(&dest_mint)?;

        // sqrt_price_limit: MIN+1 for zero_for_one, MAX-1 for one_for_zero
        // These must be strictly within bounds, not equal to the limits
        let sqrt_price_limit_x64: u128 = if zero_for_one {
            4295048017u128 // MIN_SQRT_PRICE_X64 + 1
        } else {
            79226673515401279992447579054u128 // MAX_SQRT_PRICE_X64 - 1
        };

        // Build accounts
        let mut accounts = vec![
            AccountMeta::new_readonly(self.payer.pubkey(), true),       // 0: payer
            AccountMeta::new_readonly(amm_config, false),               // 1: amm_config
            AccountMeta::new(pool.address, false),                      // 2: pool_state
            AccountMeta::new(user_input, false),                        // 3: input_token_account
            AccountMeta::new(user_output, false),                       // 4: output_token_account
            AccountMeta::new(input_vault, false),                       // 5: input_vault
            AccountMeta::new(output_vault, false),                      // 6: output_vault
            AccountMeta::new(observation_state, false),                 // 7: observation_state
            AccountMeta::new_readonly(programs::token_program(), false), // 8: token_program
        ];
        
        // Add tick arrays as remaining accounts
        for ta in &tick_arrays {
            accounts.push(AccountMeta::new(*ta, false));
        }

        // Build instruction data
        // Discriminator (8) + amount (8) + other_amount_threshold (8) + sqrt_price_limit (16) + is_base_input (1) + zero_for_one (1)
        const SWAP_DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
        
        let mut data = Vec::with_capacity(42);
        data.extend_from_slice(&SWAP_DISCRIMINATOR);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        data.extend_from_slice(&sqrt_price_limit_x64.to_le_bytes());
        data.push(1u8); // is_base_input = true
        data.push(zero_for_one as u8);

        Ok(Instruction {
            program_id: programs::raydium_clmm(),
            accounts,
            data,
        })
    }

    fn orca_whirlpool_swap_ix(
        &self,
        pool: &Pool,
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> Result<Instruction> {
        // Require CLMM-specific fields
        let tick_spacing = pool.tick_spacing
            .ok_or_else(|| anyhow!("Whirlpool missing tick_spacing"))?;
        let current_tick = pool.current_tick
            .ok_or_else(|| anyhow!("Whirlpool missing current_tick"))?;
        
        if pool.tick_arrays.is_empty() {
            return Err(anyhow!("Whirlpool has no tick arrays - run tick array discovery first"));
        }

        // Derive oracle PDA
        let oracle = programs::whirlpool_oracle_pda(&pool.address);

        // Select tick arrays for this swap direction
        // For Orca: a_to_b means tick goes DOWN, need descending arrays
        let tick_arrays = crate::pools::select_whirlpool_tick_arrays(
            &pool.tick_arrays,
            current_tick,
            tick_spacing,
            a_to_b,
        );
        
        if tick_arrays.len() < 3 {
            return Err(anyhow!("Need 3 tick arrays for Whirlpool swap, got {}", tick_arrays.len()));
        }

        // User token accounts
        let user_ata_a = self.payer_token_account(&pool.mint_a)?;
        let user_ata_b = self.payer_token_account(&pool.mint_b)?;

        // sqrt_price_limit based on direction
        const MIN_SQRT_PRICE_X64: u128 = 4295048016;
        const MAX_SQRT_PRICE_X64: u128 = 79226673515401279992447579055;
        let sqrt_price_limit: u128 = if a_to_b {
            MIN_SQRT_PRICE_X64 + 1 // Going down
        } else {
            MAX_SQRT_PRICE_X64 - 1 // Going up
        };

        // Account layout:
        // 0:  token_program
        // 1:  token_authority (signer)
        // 2:  whirlpool (writable)
        // 3:  token_owner_account_a (writable)
        // 4:  token_vault_a (writable)
        // 5:  token_owner_account_b (writable)
        // 6:  token_vault_b (writable)
        // 7:  tick_array_0 (writable)
        // 8:  tick_array_1 (writable)
        // 9:  tick_array_2 (writable)
        // 10: oracle (writable for AdaptiveFee pools)
        let accounts = vec![
            AccountMeta::new_readonly(programs::token_program(), false),
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new(pool.address, false),
            AccountMeta::new(user_ata_a, false),
            AccountMeta::new(pool.vault_a, false),
            AccountMeta::new(user_ata_b, false),
            AccountMeta::new(pool.vault_b, false),
            AccountMeta::new(tick_arrays[0], false),
            AccountMeta::new(tick_arrays[1], false),
            AccountMeta::new(tick_arrays[2], false),
            AccountMeta::new(oracle, false),
        ];

        // Build instruction data
        // Discriminator (8) + amount (8) + other_amount_threshold (8) + sqrt_price_limit (16) + amount_specified_is_input (1) + a_to_b (1)
        const SWAP_DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
        
        let mut data = Vec::with_capacity(42);
        data.extend_from_slice(&SWAP_DISCRIMINATOR);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
        data.push(1u8); // amount_specified_is_input = true
        data.push(if a_to_b { 1u8 } else { 0u8 });

        Ok(Instruction {
            program_id: programs::orca_whirlpool(),
            accounts,
            data,
        })
    }

    fn pancakeswap_clmm_swap_ix(
        &self,
        pool: &Pool,
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> Result<Instruction> {
        // PancakeSwap CLMM uses swap_v2 instruction with 17 accounts
        // Require CLMM-specific fields
        let amm_config = pool.amm_config
            .ok_or_else(|| anyhow!("PancakeSwap pool missing amm_config"))?;
        let observation_state = pool.observation_state
            .ok_or_else(|| anyhow!("PancakeSwap pool missing observation_state"))?;
        let tick_spacing = pool.tick_spacing
            .ok_or_else(|| anyhow!("PancakeSwap pool missing tick_spacing"))?;
        let current_tick = pool.current_tick
            .ok_or_else(|| anyhow!("PancakeSwap pool missing current_tick"))?;
        let bitmap_extension = pool.bitmap_extension
            .ok_or_else(|| anyhow!("PancakeSwap pool missing bitmap_extension"))?;
        
        if pool.tick_arrays.is_empty() {
            return Err(anyhow!("PancakeSwap pool has no tick arrays"));
        }

        // In PancakeSwap, a_to_b means selling token_0 for token_1 = zero_for_one = true
        let zero_for_one = a_to_b;
        
        // Select tick arrays for swap direction
        let tick_arrays = crate::pools::select_tick_arrays_for_swap(
            &pool.tick_arrays,
            current_tick,
            tick_spacing,
            zero_for_one,
        );
        
        if tick_arrays.is_empty() {
            return Err(anyhow!("No tick arrays available for swap direction"));
        }

        let (input_vault, output_vault, input_mint, output_mint) = if a_to_b {
            (pool.vault_a, pool.vault_b, pool.mint_a, pool.mint_b)
        } else {
            (pool.vault_b, pool.vault_a, pool.mint_b, pool.mint_a)
        };

        let user_input = self.payer_token_account(&input_mint)?;
        let user_output = self.payer_token_account(&output_mint)?;

        // PancakeSwap swap_v2 uses 0 for no sqrt_price_limit
        let sqrt_price_limit_x64: u128 = 0;

        // PancakeSwap CLMM account layout (17 accounts):
        // 0: payer (signer)
        // 1: amm_config
        // 2: pool_state
        // 3: input_token_account (user)
        // 4: output_token_account (user)
        // 5: input_vault
        // 6: output_vault
        // 7: observation_state
        // 8: token_program (SPL)
        // 9: token_program_2022
        // 10: memo_program
        // 11: input_vault_mint
        // 12: output_vault_mint
        // 13: bitmap_extension
        // 14-16: tick_arrays[0..3]
        let mut accounts = vec![
            AccountMeta::new_readonly(self.payer.pubkey(), true),    // 0
            AccountMeta::new_readonly(amm_config, false),            // 1
            AccountMeta::new(pool.address, false),                   // 2
            AccountMeta::new(user_input, false),                     // 3
            AccountMeta::new(user_output, false),                    // 4
            AccountMeta::new(input_vault, false),                    // 5
            AccountMeta::new(output_vault, false),                   // 6
            AccountMeta::new(observation_state, false),              // 7
            AccountMeta::new_readonly(programs::token_program(), false),      // 8
            AccountMeta::new_readonly(programs::token_program_2022(), false), // 9
            AccountMeta::new_readonly(programs::memo_program(), false),       // 10
            AccountMeta::new_readonly(input_mint, false),            // 11
            AccountMeta::new_readonly(output_mint, false),           // 12
            AccountMeta::new(bitmap_extension, false),               // 13
        ];
        
        // Add tick arrays (positions 14-16)
        for ta in tick_arrays.iter().take(3) {
            accounts.push(AccountMeta::new(*ta, false));
        }

        // swap_v2 discriminator = sha256("global:swap_v2")[0..8]
        const SWAP_V2_DISCRIMINATOR: [u8; 8] = [0x2b, 0x04, 0xed, 0x0b, 0x1a, 0xc9, 0x1e, 0x62];
        
        // swap_v2 instruction data (41 bytes):
        // discriminator (8) + amount (8) + threshold (8) + sqrt_price_limit (16) + zero_for_one (1)
        let mut data = Vec::with_capacity(41);
        data.extend_from_slice(&SWAP_V2_DISCRIMINATOR);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        data.extend_from_slice(&sqrt_price_limit_x64.to_le_bytes());
        data.push(zero_for_one as u8);

        Ok(Instruction {
            program_id: programs::pancakeswap_clmm(),
            accounts,
            data,
        })
    }

    fn meteora_damm_swap_ix(
        &self,
        pool: &Pool,
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> Result<Instruction> {
        // Meteora DAMM swap (simplified)
        let mut data = vec![1u8]; // swap instruction
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        let (source_vault, dest_vault) = if a_to_b {
            (pool.vault_a, pool.vault_b)
        } else {
            (pool.vault_b, pool.vault_a)
        };

        let (source_mint, dest_mint) = if a_to_b {
            (pool.mint_a, pool.mint_b)
        } else {
            (pool.mint_b, pool.mint_a)
        };

        let accounts = vec![
            AccountMeta::new(pool.address, false),
            AccountMeta::new(source_vault, false),
            AccountMeta::new(dest_vault, false),
            AccountMeta::new(self.payer_token_account(&source_mint)?, false),
            AccountMeta::new(self.payer_token_account(&dest_mint)?, false),
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new_readonly(programs::token_program(), false),
        ];

        Ok(Instruction {
            program_id: programs::meteora_damm(),
            accounts,
            data,
        })
    }

    fn meteora_dlmm_swap_ix(
        &self,
        pool: &Pool,
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> Result<Instruction> {
        // Meteora DLMM swap2 instruction
        // swap_for_y = true means X→Y (token_x to token_y)
        // For us: a_to_b where mint_a=token_x, mint_b=token_y → swap_for_y = a_to_b
        let swap_for_y = a_to_b;

        // Determine input/output based on direction
        let (user_token_in, user_token_out) = if a_to_b {
            (self.payer_token_account(&pool.mint_a)?, self.payer_token_account(&pool.mint_b)?)
        } else {
            (self.payer_token_account(&pool.mint_b)?, self.payer_token_account(&pool.mint_a)?)
        };

        // Derive PDAs
        let oracle = programs::dlmm_oracle_pda(&pool.address);
        let event_authority = programs::dlmm_event_authority();

        // Find bin arrays for swap
        let active_id = pool.active_bin_id.unwrap_or(0);
        let bin_arrays = self.find_dlmm_bin_arrays(&pool.address, active_id, swap_for_y);

        // Account layout for swap2:
        // 0:  lbPair (writable)
        // 1:  binArrayBitmapExtension (optional - use program as placeholder)
        // 2:  reserveX (writable)
        // 3:  reserveY (writable)
        // 4:  userTokenIn (writable)
        // 5:  userTokenOut (writable)
        // 6:  tokenXMint
        // 7:  tokenYMint
        // 8:  oracle (writable)
        // 9:  hostFeeIn (optional - use program as placeholder)
        // 10: user (signer)
        // 11: tokenXProgram
        // 12: tokenYProgram
        // 13: memoProgram
        // 14: eventAuthority
        // 15: program
        // 16+: binArrays (remaining accounts)

        let mut accounts = vec![
            AccountMeta::new(pool.address, false),
            AccountMeta::new_readonly(programs::meteora_dlmm(), false), // bitmap ext placeholder
            AccountMeta::new(pool.vault_a, false),  // reserveX
            AccountMeta::new(pool.vault_b, false),  // reserveY
            AccountMeta::new(user_token_in, false),
            AccountMeta::new(user_token_out, false),
            AccountMeta::new_readonly(pool.mint_a, false), // tokenXMint
            AccountMeta::new_readonly(pool.mint_b, false), // tokenYMint
            AccountMeta::new(oracle, false),
            AccountMeta::new_readonly(programs::meteora_dlmm(), false), // hostFeeIn placeholder
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new_readonly(programs::token_program(), false),
            AccountMeta::new_readonly(programs::token_program(), false),
            AccountMeta::new_readonly(programs::memo_program(), false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(programs::meteora_dlmm(), false),
        ];

        // Add bin arrays as remaining accounts
        for ba in &bin_arrays {
            accounts.push(AccountMeta::new(*ba, false));
        }

        // Build instruction data:
        // - discriminator: 8 bytes [0x41, 0x4B, 0x3F, 0x4C, 0xEB, 0x5B, 0x5B, 0x88]
        // - amount_in: u64 (8 bytes)
        // - min_amount_out: u64 (8 bytes)
        // - remaining_accounts_info: empty vec (4 bytes for borsh Vec length)
        const SWAP2_DISCRIMINATOR: [u8; 8] = [0x41, 0x4B, 0x3F, 0x4C, 0xEB, 0x5B, 0x5B, 0x88];

        let mut data = Vec::with_capacity(28);
        data.extend_from_slice(&SWAP2_DISCRIMINATOR);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // Empty remaining_accounts_info vec

        Ok(Instruction {
            program_id: programs::meteora_dlmm(),
            accounts,
            data,
        })
    }

    /// Find bin arrays needed for a DLMM swap
    fn find_dlmm_bin_arrays(&self, lb_pair: &Pubkey, active_id: i32, swap_for_y: bool) -> Vec<Pubkey> {
        const BINS_PER_ARRAY: i32 = 70;

        // Calculate current array index
        let current_array_idx = if active_id >= 0 {
            (active_id / BINS_PER_ARRAY) as i64
        } else {
            ((active_id + 1) / BINS_PER_ARRAY - 1) as i64
        };

        // Generate candidate indices based on direction
        let indices: Vec<i64> = if swap_for_y {
            // X→Y: bins go DOWN, need current and lower arrays
            (-3..=0).map(|offset| current_array_idx + offset).collect()
        } else {
            // Y→X: bins go UP, need current and higher arrays
            (0..=3).map(|offset| current_array_idx + offset).collect()
        };

        // Derive PDAs (we'll pass them all - contract will skip non-existent ones)
        indices.iter()
            .map(|&idx| programs::dlmm_bin_array_pda(lb_pair, idx))
            .collect()
    }

    fn tip_ix(&self, amount: u64) -> Instruction {
        // Simple SOL transfer to Jito tip account
        solana_sdk::system_instruction::transfer(
            &self.payer.pubkey(),
            &programs::jito_tip(),
            amount,
        )
    }

    /// Get or derive associated token account for payer
    fn payer_token_account(&self, mint: &Pubkey) -> Result<Pubkey> {
        Ok(spl_associated_token_account_id(&self.payer.pubkey(), mint))
    }
}

/// Derive associated token account address
fn spl_associated_token_account_id(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            owner.as_ref(),
            programs::token_program().as_ref(),
            mint.as_ref(),
        ],
        &programs::associated_token_program(),
    ).0
}

// ============================================================================
// Helius Sender - Fast Transaction Submission
// ============================================================================
// Uses Helius Sender for ultra-low latency tx submission with dual routing
// to validators and Jito infrastructure.

use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct HeliusSendRequest {
    jsonrpc: String,
    id: String,
    method: String,
    params: (String, HeliusSendOptions), // (base64 tx, options)
}

#[derive(Serialize)]
struct HeliusSendOptions {
    encoding: String,
    #[serde(rename = "skipPreflight")]
    skip_preflight: bool,
    #[serde(rename = "maxRetries")]
    max_retries: u32,
}

#[derive(Deserialize)]
struct HeliusSendResponse {
    result: Option<String>, // Transaction signature
    error: Option<serde_json::Value>,
}

/// Helius Sender client for fast transaction submission
pub struct HeliusSender {
    endpoint: String,
    http: Client,
}

impl HeliusSender {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            http: Client::new(),
        }
    }

    /// Submit a single transaction via Helius Sender
    /// Returns the transaction signature
    pub async fn send_transaction(&self, tx: &Transaction) -> Result<String> {
        use base64::Engine;
        let tx_bytes = bincode::serialize(tx)?;
        let tx_base64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);

        let request = HeliusSendRequest {
            jsonrpc: "2.0".to_string(),
            id: "1".to_string(),
            method: "sendTransaction".to_string(),
            params: (
                tx_base64,
                HeliusSendOptions {
                    encoding: "base64".to_string(),
                    skip_preflight: true,
                    max_retries: 0,
                },
            ),
        };

        let resp = self.http
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;
        
        let status = resp.status();
        let body = resp.text().await?;
        
        if !status.is_success() {
            return Err(anyhow!("HTTP {}: {}", status, body));
        }

        let response: HeliusSendResponse = serde_json::from_str(&body)
            .map_err(|e| anyhow!("Parse error: {} - body: {}", e, &body[..body.len().min(200)]))?;

        if let Some(error) = response.error {
            return Err(anyhow!("RPC error: {}", serde_json::to_string(&error).unwrap_or_default()));
        }

        response.result.ok_or_else(|| anyhow!("No signature in response: {}", &body[..body.len().min(200)]))
    }
}
