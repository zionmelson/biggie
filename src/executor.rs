//! Executor utilities for MEV bundle construction.
//!
//! Provides flash loan reserve configuration, lookup table loading, and
//! fee calculation helpers.

use crate::config::LookupTableConfig;
use crate::jito::TIP_ACCOUNTS;
use anyhow::{anyhow, Result};
use solana_address_lookup_table_interface::state::AddressLookupTable;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_message::AddressLookupTableAccount;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

/// Native SOL mint (wrapped SOL)
pub const WSOL_MINT: Pubkey = solana_sdk::pubkey!("So11111111111111111111111111111111111111112");

/// Fee constants (in basis points, 1 bp = 0.01%)
pub mod fees {
    /// Solend flash loan fee: 0.3% = 30 bps
    pub const SOLEND_FLASH_LOAN_BPS: u64 = 30;
    /// Average DEX swap fee: ~0.25% = 25 bps (varies by pool)
    pub const AVG_SWAP_FEE_BPS: u64 = 25;
    /// Slippage tolerance: 0.5% = 50 bps
    pub const DEFAULT_SLIPPAGE_BPS: u64 = 50;
    /// Jito tip minimum in lamports (absolute floor)
    pub const MIN_TIP_LAMPORTS: u64 = 1_000;
    /// Default base tip - 75th percentile of recent landed tips
    pub const BASE_TIP_LAMPORTS: u64 = 10_000;
    /// Percentage of profit to share as tip (50% = aggressive bidding)
    pub const TIP_PROFIT_SHARE_BPS: u64 = 5000;
    /// Maximum tip cap as percentage of profit (keep at least 20% profit)
    pub const MAX_TIP_PROFIT_BPS: u64 = 8000;

    /// Calculate flash loan fee in base units
    #[inline]
    pub fn calculate_flash_loan_fee(borrow_amount: u64) -> u64 {
        (borrow_amount as u128 * SOLEND_FLASH_LOAN_BPS as u128 / 10_000) as u64
    }

    /// Calculate minimum amount needed to repay flash loan (principal + fee)
    #[inline]
    pub fn flash_loan_repay_amount(borrow_amount: u64) -> u64 {
        borrow_amount.saturating_add(calculate_flash_loan_fee(borrow_amount))
    }

    /// Calculate minimum output needed to cover flash loan + buffer for safety
    #[inline]
    pub fn min_output_for_flash_loan(borrow_amount: u64, buffer_bps: u64) -> u64 {
        let repay = flash_loan_repay_amount(borrow_amount);
        repay.saturating_add((borrow_amount as u128 * buffer_bps as u128 / 10_000) as u64)
    }
}

/// Configuration for a Solend flash loan reserve.
#[derive(Clone, Debug)]
pub struct FlashLoanReserve {
    pub program_id: Pubkey,
    pub lending_market: Pubkey,
    pub reserve: Pubkey,
    pub reserve_liquidity_mint: Pubkey,
    pub borrow_liquidity_supply: Pubkey,
    pub repay_liquidity_supply: Pubkey,
    pub repay_fee_receiver: Pubkey,
    pub host_fee_receiver: Pubkey,
    /// Maximum borrow amount in base units
    pub max_borrow: u64,
    /// Minimum borrow amount in base units
    pub min_borrow: u64,
}

impl FlashLoanReserve {
    /// Derives the lending market authority PDA.
    pub fn lending_market_authority(&self) -> Pubkey {
        Pubkey::find_program_address(&[self.lending_market.as_ref()], &self.program_id).0
    }
}

/// Represents a swap that can be executed on a DEX.
#[derive(Clone, Debug)]
pub struct SwapParams {
    /// The DEX program to use
    pub program_id: Pubkey,
    /// Input token mint
    pub input_mint: Pubkey,
    /// Output token mint
    pub output_mint: Pubkey,
    /// Amount to swap (input)
    pub amount_in: u64,
    /// Minimum amount to receive (slippage protection)
    pub min_amount_out: u64,
    /// All accounts needed for the swap
    pub accounts: Vec<SwapAccount>,
    /// Instruction data (opcode + args)
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct SwapAccount {
    pub pubkey: Pubkey,
    pub is_signer: bool,
    pub is_writable: bool,
}

impl SwapParams {
    /// Converts to a Solana instruction.
    pub fn to_instruction(&self) -> Instruction {
        use solana_sdk::instruction::AccountMeta;

        let accounts: Vec<AccountMeta> = self
            .accounts
            .iter()
            .map(|a| {
                if a.is_writable {
                    AccountMeta::new(a.pubkey, a.is_signer)
                } else {
                    AccountMeta::new_readonly(a.pubkey, a.is_signer)
                }
            })
            .collect();

        Instruction {
            program_id: self.program_id,
            accounts,
            data: self.data.clone(),
        }
    }
}

/// Load Address Lookup Tables from on-chain.
///
/// Note: ALTs containing Jito tip accounts are automatically skipped to avoid
/// issues with bundle validation.
pub async fn load_lookup_tables(
    rpc_client: &RpcClient,
    configs: &[LookupTableConfig],
    _payer: &Pubkey,
) -> Result<Vec<AddressLookupTableAccount>> {
    let mut tables = Vec::new();

    // Tip accounts that must NOT be in any ALT we use
    let tip_pubkeys: Vec<Pubkey> = TIP_ACCOUNTS
        .iter()
        .filter_map(|s| Pubkey::from_str(s).ok())
        .collect();

    for config in configs {
        let address = Pubkey::from_str(&config.address)
            .map_err(|e| anyhow!("invalid ALT address '{}': {}", config.address, e))?;

        match rpc_client.get_account(&address).await {
            Ok(account) => match AddressLookupTable::deserialize(&account.data) {
                Ok(lookup_table) => {
                    // Check if this ALT contains any tip accounts
                    let contains_tip = lookup_table
                        .addresses
                        .iter()
                        .any(|addr| tip_pubkeys.contains(addr));

                    if contains_tip {
                        tracing::warn!(
                            "SKIPPING ALT '{}' ({}) - contains Jito tip accounts!",
                            config.label,
                            address
                        );
                        continue;
                    }

                    let alt_account = AddressLookupTableAccount {
                        key: address,
                        addresses: lookup_table.addresses.to_vec(),
                    };
                    tracing::info!(
                        "Loaded ALT '{}' ({}) with {} addresses",
                        config.label,
                        address,
                        alt_account.addresses.len(),
                    );
                    tables.push(alt_account);
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to deserialize ALT '{}' ({}): {:?}",
                        config.label,
                        address,
                        e
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Failed to fetch ALT '{}' ({}): {}",
                    config.label,
                    address,
                    e
                );
            }
        }
    }

    Ok(tables)
}
