//! Transaction parser for extracting swap information from DEX transactions.
//!
//! Parses VersionedTransactions to identify swaps and extract key parameters.

use solana_message::compiled_instruction::CompiledInstruction;
use solana_message::v0::LoadedAddresses;
use solana_message::VersionedMessage;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::VersionedTransaction;
use std::time::Instant;

use super::programs::{get_dex_type, is_swap_instruction, pool_account_index, DexType};

/// Direction of a swap
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapDirection {
    /// Swap token A for token B
    AtoB,
    /// Swap token B for token A
    BtoA,
}

/// A parsed swap from a transaction
#[derive(Debug, Clone)]
pub struct ParsedSwap {
    /// The DEX this swap is on
    pub dex: DexType,
    /// Pool address (AMM or Whirlpool)
    pub pool: Pubkey,
    /// Token mint being sold (input)
    pub input_mint: Option<Pubkey>,
    /// Token mint being bought (output)
    pub output_mint: Option<Pubkey>,
    /// Input amount (if parseable)
    pub amount_in: Option<u64>,
    /// Minimum output amount / slippage threshold (if parseable)
    /// For ExactIn: minimum output. For ExactOut: maximum input.
    pub min_amount_out: Option<u64>,
    /// Swap direction
    pub direction: Option<SwapDirection>,
    /// The user/signer initiating the swap
    pub user: Pubkey,
    /// Index of the swap instruction in the transaction
    pub instruction_index: usize,
    /// Is the swap specified by input amount (ExactIn=true) or output (ExactOut=false)?
    /// Critical for sandwich: ExactIn swaps are more exploitable.
    pub amount_specified_is_input: Option<bool>,
    /// Price limit (sqrt_price for CLMMs). 0 = no limit = maximum exploitable slippage.
    pub sqrt_price_limit: Option<u128>,
}

/// Result of parsing a transaction for swaps
#[derive(Debug)]
pub struct TransactionSwaps {
    /// The transaction signature (first 8 bytes for logging)
    pub signature_prefix: [u8; 8],
    /// Slot where this transaction was seen
    pub slot: u64,
    /// When we received this transaction
    pub received_at: Instant,
    /// Parsed swaps in this transaction
    pub swaps: Vec<ParsedSwap>,
    /// The original transaction (for bundle construction)
    pub transaction: VersionedTransaction,
}

/// Parse a VersionedTransaction to extract swap information
pub fn parse_transaction(
    transaction: &VersionedTransaction,
    slot: u64,
    received_at: Instant,
    loaded_addresses: Option<&LoadedAddresses>,
) -> Option<TransactionSwaps> {
    let message = &transaction.message;
    let account_keys = get_account_keys(message, loaded_addresses);
    
    // Get the signer (first account)
    let user = account_keys.first()?.clone();
    
    let mut swaps = Vec::new();
    
    // Log program IDs being called for debugging (sample 1% of txs)
    static LOG_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = LOG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let should_log = count % 100 == 0;
    
    // Iterate through instructions looking for swaps
    for (idx, instruction) in message.instructions().iter().enumerate() {
        // Check if this is a DEX program
        if let Some(program_id) = account_keys.get(instruction.program_id_index as usize) {
            if should_log {
                use super::programs::{get_dex_type, is_swap_instruction};
                let is_dex = get_dex_type(program_id).is_some();
                let is_swap = is_swap_instruction(program_id, &instruction.data);
                if is_dex {
                    tracing::info!(
                        program = %program_id,
                        is_swap,
                        data_len = instruction.data.len(),
                        "DEX program called in transaction"
                    );
                }
            }
        }
        
        if let Some(swap) = parse_instruction(instruction, &account_keys, &user, idx) {
            swaps.push(swap);
        }
    }
    
    if swaps.is_empty() {
        return None;
    }
    
    // Extract signature prefix for logging
    let sig_bytes = transaction.signatures.first()?.as_ref();
    let mut signature_prefix = [0u8; 8];
    signature_prefix.copy_from_slice(&sig_bytes[..8]);
    
    Some(TransactionSwaps {
        signature_prefix,
        slot,
        received_at,
        swaps,
        transaction: transaction.clone(),
    })
}

/// Get all account keys including loaded addresses (for v0 transactions)
fn get_account_keys(
    message: &VersionedMessage,
    loaded_addresses: Option<&LoadedAddresses>,
) -> Vec<Pubkey> {
    let static_keys = message.static_account_keys();
    
    match loaded_addresses {
        Some(loaded) => {
            let mut keys: Vec<Pubkey> = static_keys.to_vec();
            keys.extend(loaded.writable.iter().cloned());
            keys.extend(loaded.readonly.iter().cloned());
            keys
        }
        None => static_keys.to_vec(),
    }
}

/// Extract pool address from instruction using the centralized pool_account_index
fn get_pool_from_instruction(
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    dex: DexType,
) -> Option<Pubkey> {
    let pool_idx = pool_account_index(dex);
    let account_idx = *instruction.accounts.get(pool_idx)? as usize;
    account_keys.get(account_idx).copied()
}

/// Parse a single instruction for swap information
fn parse_instruction(
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    user: &Pubkey,
    instruction_index: usize,
) -> Option<ParsedSwap> {
    let program_id = account_keys.get(instruction.program_id_index as usize)?;
    
    // Check if this is a known DEX program
    let dex = get_dex_type(program_id)?;
    
    // Check if this is a swap instruction
    if !is_swap_instruction(program_id, &instruction.data) {
        return None;
    }
    
    // Parse based on DEX type
    match dex {
        DexType::RaydiumAmm => parse_raydium_amm_swap(instruction, account_keys, user, instruction_index),
        DexType::RaydiumClmm => parse_raydium_clmm_swap(instruction, account_keys, user, instruction_index),
        DexType::OrcaWhirlpool => parse_orca_whirlpool_swap(instruction, account_keys, user, instruction_index),
        DexType::MeteoraDlmm => parse_meteora_dlmm_swap(instruction, account_keys, user, instruction_index),
        DexType::MeteoraDamm => parse_meteora_damm_swap(instruction, account_keys, user, instruction_index),
        DexType::PancakeSwapClmm => parse_pancakeswap_clmm_swap(instruction, account_keys, user, instruction_index),
    }
}

// ============================================================================
// DEX-specific parsers
// ============================================================================

/// Parse Raydium AMM swap instruction
/// Account layout for swap_base_in/swap_base_out:
/// 0: token_program, 1: amm_id, 2: amm_authority, 3: amm_open_orders,
/// 4: amm_target_orders, 5: pool_coin_token_account, 6: pool_pc_token_account,
/// 7: serum_program_id, 8: serum_market, 9: serum_bids, 10: serum_asks,
/// 11: serum_event_queue, 12: serum_coin_vault, 13: serum_pc_vault,
/// 14: serum_vault_signer, 15: user_source_token, 16: user_dest_token, 17: user_owner
fn parse_raydium_amm_swap(
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    user: &Pubkey,
    instruction_index: usize,
) -> Option<ParsedSwap> {
    // Use centralized pool extraction
    let pool = get_pool_from_instruction(instruction, account_keys, DexType::RaydiumAmm)?;
    
    // Parse amount from instruction data
    // Format: [instruction_type(1), amount_in(8), min_amount_out(8)]
    // instruction_type: 9 = swap_base_in (ExactIn), 10 = swap_base_out (ExactOut)
    let data = &instruction.data;
    
    let amount_specified_is_input = if !data.is_empty() {
        Some(data[0] == 9) // 9 = swap_base_in, 10 = swap_base_out
    } else {
        None
    };
    
    let amount_in = if data.len() >= 9 {
        Some(u64::from_le_bytes(data[1..9].try_into().ok()?))
    } else {
        None
    };
    
    let min_amount_out = if data.len() >= 17 {
        Some(u64::from_le_bytes(data[9..17].try_into().ok()?))
    } else {
        None
    };
    
    Some(ParsedSwap {
        dex: DexType::RaydiumAmm,
        pool,
        input_mint: None, // Would need to lookup from pool state
        output_mint: None,
        amount_in,
        min_amount_out,
        direction: None, // Would need pool state to determine
        user: user.clone(),
        instruction_index,
        amount_specified_is_input,
        sqrt_price_limit: None, // AMM doesn't have price limits
    })
}

/// Parse Raydium CLMM swap instruction
/// Account layout for swap:
/// 0: payer, 1: amm_config, 2: pool_state, 3: input_token_account,
/// 4: output_token_account, 5: input_vault, 6: output_vault,
/// 7: observation_state, 8: token_program, 9: tick_array_...
fn parse_raydium_clmm_swap(
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    user: &Pubkey,
    instruction_index: usize,
) -> Option<ParsedSwap> {
    // Use centralized pool extraction
    let pool = get_pool_from_instruction(instruction, account_keys, DexType::RaydiumClmm)?;
    
    // Parse Anchor instruction data: [discriminator(8), amount(8), other_amount_threshold(8), sqrt_price_limit(16), is_base_input(1)]
    let data = &instruction.data;
    let amount_in = if data.len() >= 16 {
        Some(u64::from_le_bytes(data[8..16].try_into().ok()?))
    } else {
        None
    };
    
    let min_amount_out = if data.len() >= 24 {
        Some(u64::from_le_bytes(data[16..24].try_into().ok()?))
    } else {
        None
    };
    
    // sqrt_price_limit is u128 at bytes 24..40
    let sqrt_price_limit = if data.len() >= 40 {
        Some(u128::from_le_bytes(data[24..40].try_into().ok()?))
    } else {
        None
    };
    
    // is_base_input (amount_specified_is_input) at byte 40
    let amount_specified_is_input = if data.len() >= 41 {
        Some(data[40] == 1)
    } else {
        None
    };
    
    Some(ParsedSwap {
        dex: DexType::RaydiumClmm,
        pool,
        input_mint: None,
        output_mint: None,
        amount_in,
        min_amount_out,
        direction: None,
        user: user.clone(),
        instruction_index,
        amount_specified_is_input,
        sqrt_price_limit,
    })
}

/// Parse Orca Whirlpool swap instruction
/// Account layout for swap:
/// 0: token_program, 1: token_authority, 2: whirlpool, 3: token_owner_account_a,
/// 4: token_vault_a, 5: token_owner_account_b, 6: token_vault_b,
/// 7: tick_array_0, 8: tick_array_1, 9: tick_array_2, 10: oracle
fn parse_orca_whirlpool_swap(
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    user: &Pubkey,
    instruction_index: usize,
) -> Option<ParsedSwap> {
    // Use centralized pool extraction
    let pool = get_pool_from_instruction(instruction, account_keys, DexType::OrcaWhirlpool)?;
    
    // Parse instruction data: [discriminator(8), amount(8), other_amount_threshold(8), sqrt_price_limit(16), amount_specified_is_input(1), a_to_b(1)]
    let data = &instruction.data;
    let amount_in = if data.len() >= 16 {
        Some(u64::from_le_bytes(data[8..16].try_into().ok()?))
    } else {
        None
    };
    
    let min_amount_out = if data.len() >= 24 {
        Some(u64::from_le_bytes(data[16..24].try_into().ok()?))
    } else {
        None
    };
    
    // sqrt_price_limit is u128 at bytes 24..40
    let sqrt_price_limit = if data.len() >= 40 {
        Some(u128::from_le_bytes(data[24..40].try_into().ok()?))
    } else {
        None
    };
    
    // amount_specified_is_input at byte 40
    let amount_specified_is_input = if data.len() >= 41 {
        Some(data[40] == 1)
    } else {
        None
    };
    
    // a_to_b flag is at byte 41
    let direction = if data.len() >= 42 {
        Some(if data[41] == 1 { SwapDirection::AtoB } else { SwapDirection::BtoA })
    } else {
        None
    };
    
    Some(ParsedSwap {
        dex: DexType::OrcaWhirlpool,
        pool,
        input_mint: None,
        output_mint: None,
        amount_in,
        min_amount_out,
        direction,
        user: user.clone(),
        instruction_index,
        amount_specified_is_input,
        sqrt_price_limit,
    })
}

/// Parse Meteora DLMM swap instruction
/// Layout: [discriminator(8), amount_in(8), min_amount_out(8)]
/// DLMM uses bin-based pricing, no sqrt_price_limit concept
fn parse_meteora_dlmm_swap(
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    user: &Pubkey,
    instruction_index: usize,
) -> Option<ParsedSwap> {
    // Use centralized pool extraction
    let pool = get_pool_from_instruction(instruction, account_keys, DexType::MeteoraDlmm)?;
    
    let data = &instruction.data;
    let amount_in = if data.len() >= 16 {
        Some(u64::from_le_bytes(data[8..16].try_into().ok()?))
    } else {
        None
    };
    
    let min_amount_out = if data.len() >= 24 {
        Some(u64::from_le_bytes(data[16..24].try_into().ok()?))
    } else {
        None
    };
    
    Some(ParsedSwap {
        dex: DexType::MeteoraDlmm,
        pool,
        input_mint: None,
        output_mint: None,
        amount_in,
        min_amount_out,
        direction: None,
        user: user.clone(),
        instruction_index,
        amount_specified_is_input: Some(true), // DLMM swap is always ExactIn
        sqrt_price_limit: None, // DLMM uses bins, not sqrt_price
    })
}

/// Parse Meteora DAMM v2 swap instruction
/// DAMM is a constant-product AMM, always ExactIn
fn parse_meteora_damm_swap(
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    user: &Pubkey,
    instruction_index: usize,
) -> Option<ParsedSwap> {
    // Use centralized pool extraction
    let pool = get_pool_from_instruction(instruction, account_keys, DexType::MeteoraDamm)?;
    
    let data = &instruction.data;
    let amount_in = if data.len() >= 16 {
        Some(u64::from_le_bytes(data[8..16].try_into().ok()?))
    } else {
        None
    };
    
    let min_amount_out = if data.len() >= 24 {
        Some(u64::from_le_bytes(data[16..24].try_into().ok()?))
    } else {
        None
    };
    
    Some(ParsedSwap {
        dex: DexType::MeteoraDamm,
        pool,
        input_mint: None,
        output_mint: None,
        amount_in,
        min_amount_out,
        direction: None,
        user: user.clone(),
        instruction_index,
        amount_specified_is_input: Some(true), // DAMM swap is always ExactIn
        sqrt_price_limit: None, // DAMM is constant-product, no price limit
    })
}

/// Parse PancakeSwap CLMM swap instruction (similar to Raydium CLMM)
/// Layout: [discriminator(8), amount(8), other_amount_threshold(8), sqrt_price_limit(16), is_base_input(1)]
fn parse_pancakeswap_clmm_swap(
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    user: &Pubkey,
    instruction_index: usize,
) -> Option<ParsedSwap> {
    // Use centralized pool extraction
    let pool = get_pool_from_instruction(instruction, account_keys, DexType::PancakeSwapClmm)?;
    
    let data = &instruction.data;
    let amount_in = if data.len() >= 16 {
        Some(u64::from_le_bytes(data[8..16].try_into().ok()?))
    } else {
        None
    };
    
    let min_amount_out = if data.len() >= 24 {
        Some(u64::from_le_bytes(data[16..24].try_into().ok()?))
    } else {
        None
    };
    
    // sqrt_price_limit is u128 at bytes 24..40
    let sqrt_price_limit = if data.len() >= 40 {
        Some(u128::from_le_bytes(data[24..40].try_into().ok()?))
    } else {
        None
    };
    
    // is_base_input at byte 40
    let amount_specified_is_input = if data.len() >= 41 {
        Some(data[40] == 1)
    } else {
        None
    };
    
    Some(ParsedSwap {
        dex: DexType::PancakeSwapClmm,
        pool,
        input_mint: None,
        output_mint: None,
        amount_in,
        min_amount_out,
        direction: None,
        user: user.clone(),
        instruction_index,
        amount_specified_is_input,
        sqrt_price_limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_swap_direction() {
        assert_eq!(SwapDirection::AtoB, SwapDirection::AtoB);
        assert_ne!(SwapDirection::AtoB, SwapDirection::BtoA);
    }
}
