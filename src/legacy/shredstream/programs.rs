//! DEX program IDs and instruction discriminators for swap detection.
//!
//! All program IDs verified from source repositories - see docs/arb_strategy.md

use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::LazyLock;

/// Raydium AMM V4 Program
pub static RAYDIUM_AMM: LazyLock<Pubkey> = LazyLock::new(|| {
    Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap()
});

/// Raydium CLMM (Concentrated Liquidity) Program
pub static RAYDIUM_CLMM: LazyLock<Pubkey> = LazyLock::new(|| {
    Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").unwrap()
});

/// Orca Whirlpool Program
pub static ORCA_WHIRLPOOL: LazyLock<Pubkey> = LazyLock::new(|| {
    Pubkey::from_str("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc").unwrap()
});

/// Meteora DLMM Program
pub static METEORA_DLMM: LazyLock<Pubkey> = LazyLock::new(|| {
    Pubkey::from_str("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo").unwrap()
});

/// Meteora DAMM v2 (CP-AMM) Program
pub static METEORA_DAMM: LazyLock<Pubkey> = LazyLock::new(|| {
    Pubkey::from_str("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG").unwrap()
});

/// PancakeSwap CLMM Solana Program
pub static PANCAKESWAP_CLMM: LazyLock<Pubkey> = LazyLock::new(|| {
    Pubkey::from_str("HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq").unwrap()
});

/// DEX type for categorizing pools
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DexType {
    RaydiumAmm,
    RaydiumClmm,
    OrcaWhirlpool,
    MeteoraDlmm,
    MeteoraDamm,
    PancakeSwapClmm,
}

impl DexType {
    /// Get the program ID for this DEX
    pub fn program_id(&self) -> Pubkey {
        match self {
            DexType::RaydiumAmm => *RAYDIUM_AMM,
            DexType::RaydiumClmm => *RAYDIUM_CLMM,
            DexType::OrcaWhirlpool => *ORCA_WHIRLPOOL,
            DexType::MeteoraDlmm => *METEORA_DLMM,
            DexType::MeteoraDamm => *METEORA_DAMM,
            DexType::PancakeSwapClmm => *PANCAKESWAP_CLMM,
        }
    }

    /// Check if this is a CLMM (concentrated liquidity)
    pub fn is_clmm(&self) -> bool {
        matches!(
            self,
            DexType::RaydiumClmm
                | DexType::OrcaWhirlpool
                | DexType::MeteoraDlmm
                | DexType::PancakeSwapClmm
        )
    }
}

/// Lookup table: Program ID -> DexType
pub static PROGRAM_TO_DEX: LazyLock<HashMap<Pubkey, DexType>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    map.insert(*RAYDIUM_AMM, DexType::RaydiumAmm);
    map.insert(*RAYDIUM_CLMM, DexType::RaydiumClmm);
    map.insert(*ORCA_WHIRLPOOL, DexType::OrcaWhirlpool);
    map.insert(*METEORA_DLMM, DexType::MeteoraDlmm);
    map.insert(*METEORA_DAMM, DexType::MeteoraDamm);
    map.insert(*PANCAKESWAP_CLMM, DexType::PancakeSwapClmm);
    map
});

// ============================================================================
// Instruction Discriminators (first 8 bytes of instruction data)
// ============================================================================

/// Raydium AMM swap discriminators
pub mod raydium_amm {
    /// swap_base_in instruction discriminator (instruction index 9)
    pub const SWAP_BASE_IN: u8 = 9;
    /// swap_base_out instruction discriminator (instruction index 11)
    pub const SWAP_BASE_OUT: u8 = 11;
}

/// Raydium CLMM swap discriminators (Anchor)
/// From: raydium-clmm/programs/amm/src/lib.rs
pub mod raydium_clmm {
    /// swap instruction: sighash("global:swap")
    pub const SWAP: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
    /// swap_v2 instruction (with remaining accounts for Token-2022)
    pub const SWAP_V2: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];
}

/// Orca Whirlpool swap discriminators (Anchor)
/// From: whirlpools/programs/whirlpool/src/lib.rs
pub mod orca_whirlpool {
    /// swap instruction: sighash("global:swap")
    pub const SWAP: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
    /// swap_v2 (Token-2022 support)
    pub const SWAP_V2: [u8; 8] = [0x2b, 0x04, 0xed, 0x0b, 0x1a, 0xc9, 0x1e, 0x62];
    /// two_hop_swap
    pub const TWO_HOP_SWAP: [u8; 8] = [0xc3, 0x60, 0xed, 0x6c, 0x44, 0xa2, 0xdb, 0xe6];
}

/// Meteora DLMM swap discriminators (Anchor)
/// From: dlmm-sdk/ts-client/src/dlmm/idl.ts
pub mod meteora_dlmm {
    /// swap instruction: sighash("global:swap")
    pub const SWAP: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
    /// swap2 instruction (with remaining_accounts_info for bin arrays)
    pub const SWAP2: [u8; 8] = [0x41, 0x4b, 0x3f, 0x4c, 0xeb, 0x5b, 0x5b, 0x88];
}

/// Meteora DAMM v2 (CP-AMM) swap discriminators (Anchor)
/// From: damm-v2/programs/cp-amm/src/lib.rs
pub mod meteora_damm {
    /// swap instruction: sighash("global:swap")
    pub const SWAP: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
    /// swap2 instruction (supports ExactIn, PartialFill, ExactOut modes)
    pub const SWAP2: [u8; 8] = [0x5c, 0xb7, 0x8b, 0x6c, 0xc2, 0xf1, 0x9a, 0x43];
}

/// PancakeSwap CLMM swap discriminators (similar to Raydium CLMM)
/// From: pancake-frontend/packages/solana-core-sdk
pub mod pancakeswap_clmm {
    /// swap instruction
    pub const SWAP: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];
}

// ============================================================================
// Discriminator Matching
// ============================================================================

/// Check if instruction data starts with a swap discriminator
pub fn is_swap_instruction(program_id: &Pubkey, data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }

    match PROGRAM_TO_DEX.get(program_id) {
        Some(DexType::RaydiumAmm) => {
            // Raydium AMM uses single-byte instruction index
            data[0] == raydium_amm::SWAP_BASE_IN || data[0] == raydium_amm::SWAP_BASE_OUT
        }
        Some(DexType::RaydiumClmm) => {
            data.len() >= 8
                && (data[..8] == raydium_clmm::SWAP || data[..8] == raydium_clmm::SWAP_V2)
        }
        Some(DexType::OrcaWhirlpool) => {
            data.len() >= 8
                && (data[..8] == orca_whirlpool::SWAP
                    || data[..8] == orca_whirlpool::SWAP_V2
                    || data[..8] == orca_whirlpool::TWO_HOP_SWAP)
        }
        Some(DexType::MeteoraDlmm) => {
            data.len() >= 8
                && (data[..8] == meteora_dlmm::SWAP || data[..8] == meteora_dlmm::SWAP2)
        }
        Some(DexType::MeteoraDamm) => {
            data.len() >= 8
                && (data[..8] == meteora_damm::SWAP || data[..8] == meteora_damm::SWAP2)
        }
        Some(DexType::PancakeSwapClmm) => {
            data.len() >= 8 && data[..8] == pancakeswap_clmm::SWAP
        }
        None => false,
    }
}

/// Get DEX type from program ID
pub fn get_dex_type(program_id: &Pubkey) -> Option<DexType> {
    PROGRAM_TO_DEX.get(program_id).copied()
}

/// Check if this program ID is a known DEX
pub fn is_dex_program(program_id: &Pubkey) -> bool {
    PROGRAM_TO_DEX.contains_key(program_id)
}

// ============================================================================
// Pool Address Extraction
// ============================================================================

/// Get the account index where the pool address is located for swap instructions.
///
/// This centralizes the knowledge of pool address location across all DEXs:
/// - Raydium AMM: account[1] is the AMM pool
/// - Raydium CLMM: account[2] is the pool state  
/// - Orca Whirlpool: account[2] is the whirlpool
/// - Meteora DLMM: account[1] is the lb_pair (verified from parser.rs layout)
/// - Meteora DAMM: account[1] is the pool (verified from parser.rs layout)
/// - PancakeSwap CLMM: account[2] is the pool state (similar to Raydium CLMM)
pub fn pool_account_index(dex: DexType) -> usize {
    match dex {
        DexType::RaydiumAmm => 1,
        DexType::RaydiumClmm => 2,
        DexType::OrcaWhirlpool => 2,
        DexType::MeteoraDlmm => 1,
        DexType::MeteoraDamm => 1,
        DexType::PancakeSwapClmm => 2,
    }
}

/// Extract pool address from instruction account indices
///
/// Generic helper that works with any account lookup mechanism.
/// Returns the pool address if valid, None otherwise.
pub fn extract_pool_address<F>(dex: DexType, get_account: F) -> Option<Pubkey>
where
    F: Fn(usize) -> Option<Pubkey>,
{
    let idx = pool_account_index(dex);
    get_account(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_program_ids_valid() {
        // Ensure all program IDs parse correctly
        assert_eq!(
            RAYDIUM_AMM.to_string(),
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"
        );
        assert_eq!(
            RAYDIUM_CLMM.to_string(),
            "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK"
        );
        assert_eq!(
            ORCA_WHIRLPOOL.to_string(),
            "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc"
        );
        assert_eq!(
            METEORA_DLMM.to_string(),
            "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo"
        );
        assert_eq!(
            METEORA_DAMM.to_string(),
            "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG"
        );
        assert_eq!(
            PANCAKESWAP_CLMM.to_string(),
            "HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq"
        );
    }

    #[test]
    fn test_is_swap_instruction_raydium_amm() {
        let data = [9u8, 0, 0, 0, 0]; // swap_base_in
        assert!(is_swap_instruction(&RAYDIUM_AMM, &data));

        let data = [11u8, 0, 0, 0, 0]; // swap_base_out
        assert!(is_swap_instruction(&RAYDIUM_AMM, &data));

        let data = [1u8, 0, 0, 0, 0]; // not a swap
        assert!(!is_swap_instruction(&RAYDIUM_AMM, &data));
    }

    #[test]
    fn test_is_swap_instruction_raydium_clmm() {
        let mut data = vec![0u8; 32];
        data[..8].copy_from_slice(&raydium_clmm::SWAP);
        assert!(is_swap_instruction(&RAYDIUM_CLMM, &data));
    }
}
