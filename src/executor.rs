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

// Re-export for backward compatibility
pub use crate::constants::{
    calculate_flash_loan_fee, flash_loan_repay_amount, min_output_for_flash_loan, AVG_SWAP_FEE_BPS,
    BASE_TIP_LAMPORTS, DEFAULT_SLIPPAGE_BPS, MAX_TIP_PROFIT_BPS, MIN_TIP_LAMPORTS,
    SOLEND_FLASH_LOAN_BPS, TIP_PROFIT_SHARE_BPS, WSOL_MINT,
};

/// Fee constants module - re-exports from constants for backward compatibility
pub mod fees {
    pub use crate::constants::{
        calculate_flash_loan_fee, flash_loan_repay_amount, min_output_for_flash_loan,
        AVG_SWAP_FEE_BPS, BASE_TIP_LAMPORTS, DEFAULT_SLIPPAGE_BPS, MAX_TIP_PROFIT_BPS,
        MIN_TIP_LAMPORTS, SOLEND_FLASH_LOAN_BPS, TIP_PROFIT_SHARE_BPS,
    };
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
