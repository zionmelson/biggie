//! Dynamic Pool Discovery via Geyser
//!
//! Replaces static JSON files with live on-chain data:
//! - Discovers CLMM/DLMM pools by subscribing to program accounts
//! - Parses pool accounts to extract tick_spacing/bin_step
//! - Tracks pool state changes in real-time
//!
//! Supported protocols:
//! - Orca Whirlpool: tick_spacing from Whirlpool account
//! - Raydium CLMM: tick_spacing from PoolState account
//! - Meteora DLMM: bin_step from LbPair account
//! - PancakeSwap CLMM: tick_spacing from PoolState account

use crate::shredstream::programs::{
    DexType, METEORA_DAMM, METEORA_DLMM, ORCA_WHIRLPOOL, PANCAKESWAP_CLMM, RAYDIUM_AMM,
    RAYDIUM_CLMM,
};
use solana_sdk::pubkey::Pubkey;

// ============================================================================
// Account Layout Offsets (verified from IDLs/source)
// ============================================================================

/// Orca Whirlpool account layout
/// Source: https://github.com/orca-so/whirlpools/blob/main/programs/whirlpool/src/state/whirlpool.rs
mod whirlpool_layout {
    /// Discriminator (8 bytes)
    pub const DISCRIMINATOR: usize = 0;
    /// WhirlpoolsConfig pubkey
    pub const CONFIG: usize = 8;
    /// WhirlpoolBump (1 byte)
    pub const BUMP: usize = 40;
    /// tick_spacing (u16) - THE KEY VALUE
    pub const TICK_SPACING: usize = 41;
    /// tick_spacing_seed (2 bytes)
    pub const TICK_SPACING_SEED: usize = 43;
    /// fee_rate (u16)
    pub const FEE_RATE: usize = 45;
    /// protocol_fee_rate (u16)
    pub const PROTOCOL_FEE_RATE: usize = 47;
    /// liquidity (u128)
    pub const LIQUIDITY: usize = 49;
    /// sqrt_price (u128)
    pub const SQRT_PRICE: usize = 65;
    /// tick_current_index (i32)
    pub const TICK_CURRENT_INDEX: usize = 81;
    /// protocol_fee_owed_a (u64)
    pub const PROTOCOL_FEE_OWED_A: usize = 85;
    /// protocol_fee_owed_b (u64)
    pub const PROTOCOL_FEE_OWED_B: usize = 93;
    /// token_mint_a (pubkey)
    pub const TOKEN_MINT_A: usize = 101;
    /// token_vault_a (pubkey)
    pub const TOKEN_VAULT_A: usize = 133;
    /// fee_growth_global_a (u128)
    pub const FEE_GROWTH_A: usize = 165;
    /// token_mint_b (pubkey)
    pub const TOKEN_MINT_B: usize = 181;
    /// token_vault_b (pubkey)
    pub const TOKEN_VAULT_B: usize = 213;
    /// Minimum account size
    pub const MIN_SIZE: usize = 245;
}

/// Raydium CLMM PoolState layout
/// Source: https://github.com/raydium-io/raydium-clmm/blob/master/programs/amm/src/states/pool.rs
mod raydium_clmm_layout {
    /// Discriminator (8 bytes)
    pub const DISCRIMINATOR: usize = 0;
    /// bump (1 byte)
    pub const BUMP: usize = 8;
    /// amm_config (pubkey)
    pub const AMM_CONFIG: usize = 9;
    /// owner (pubkey)
    pub const OWNER: usize = 41;
    /// token_mint_0 (pubkey)
    pub const TOKEN_MINT_0: usize = 73;
    /// token_mint_1 (pubkey)
    pub const TOKEN_MINT_1: usize = 105;
    /// token_vault_0 (pubkey)
    pub const TOKEN_VAULT_0: usize = 137;
    /// token_vault_1 (pubkey)
    pub const TOKEN_VAULT_1: usize = 169;
    /// observation_key (pubkey)
    pub const OBSERVATION_KEY: usize = 201;
    /// mint_decimals_0 (u8)
    pub const DECIMALS_0: usize = 233;
    /// mint_decimals_1 (u8)
    pub const DECIMALS_1: usize = 234;
    /// tick_spacing (u16) - THE KEY VALUE
    pub const TICK_SPACING: usize = 235;
    /// liquidity (u128)
    pub const LIQUIDITY: usize = 237;
    /// sqrt_price_x64 (u128)
    pub const SQRT_PRICE: usize = 253;
    /// tick_current (i32)
    pub const TICK_CURRENT: usize = 269;
    /// Minimum account size
    pub const MIN_SIZE: usize = 273;
}

/// Meteora DLMM LbPair layout
/// Source: https://github.com/MeteoraAg/dlmm-sdk/blob/main/programs/lb_clmm/src/state/lb_pair.rs
mod dlmm_layout {
    /// Discriminator (8 bytes)
    pub const DISCRIMINATOR: usize = 0;
    /// parameters (various fields packed)
    pub const PARAMETERS_START: usize = 8;
    /// base_factor (u16) at offset 8
    pub const BASE_FACTOR: usize = 8;
    /// filter_period (u16) at offset 10
    pub const FILTER_PERIOD: usize = 10;
    /// decay_period (u16) at offset 12
    pub const DECAY_PERIOD: usize = 12;
    /// reduction_factor (u16) at offset 14
    pub const REDUCTION_FACTOR: usize = 14;
    /// variable_fee_control (u32) at offset 16
    pub const VAR_FEE_CONTROL: usize = 16;
    /// max_volatility_accumulator (u32) at offset 20
    pub const MAX_VOL_ACC: usize = 20;
    /// min_bin_id (i32) at offset 24
    pub const MIN_BIN_ID: usize = 24;
    /// max_bin_id (i32) at offset 28
    pub const MAX_BIN_ID: usize = 28;
    /// protocol_share (u16) at offset 32
    pub const PROTOCOL_SHARE: usize = 32;
    /// padding (6 bytes)
    /// static_parameters end
    /// v_parameters start at 40
    pub const V_PARAMETERS: usize = 40;
    /// active_id (i32) at offset 72
    pub const ACTIVE_ID: usize = 72;
    /// bin_step (u16) at offset 76 - THE KEY VALUE
    pub const BIN_STEP: usize = 76;
    /// status (u8) at offset 78
    pub const STATUS: usize = 78;
    /// pair_type (u8) at offset 79
    pub const PAIR_TYPE: usize = 79;
    /// lbPairType (u8) at offset 80
    pub const LB_PAIR_TYPE: usize = 80;
    /// padding
    /// token_x_mint (pubkey) at offset ~88 (depends on version)
    pub const TOKEN_X_MINT: usize = 88;
    /// token_y_mint (pubkey)
    pub const TOKEN_Y_MINT: usize = 120;
    /// reserve_x (pubkey)
    pub const RESERVE_X: usize = 152;
    /// reserve_y (pubkey)
    pub const RESERVE_Y: usize = 184;
    /// Minimum account size
    pub const MIN_SIZE: usize = 216;
}

/// Raydium AMM (constant product) layout
/// Source: https://github.com/raydium-io/raydium-amm/blob/master/program/src/state.rs
mod raydium_amm_layout {
    /// status (u64)
    pub const STATUS: usize = 0;
    /// nonce (u64)
    pub const NONCE: usize = 8;
    /// max_order (u64)
    pub const MAX_ORDER: usize = 16;
    /// depth (u64)
    pub const DEPTH: usize = 24;
    /// base_decimal (u64)
    pub const BASE_DECIMAL: usize = 32;
    /// quote_decimal (u64)
    pub const QUOTE_DECIMAL: usize = 40;
    /// state (u64)
    pub const STATE: usize = 48;
    /// reset_flag (u64)
    pub const RESET_FLAG: usize = 56;
    /// min_size (u64)
    pub const MIN_SIZE_FIELD: usize = 64;
    /// vol_max_cut_ratio (u64)
    pub const VOL_MAX_CUT: usize = 72;
    /// amount_wave_ratio (u64)
    pub const AMOUNT_WAVE: usize = 80;
    /// base_lot_size (u64)
    pub const BASE_LOT_SIZE: usize = 88;
    /// quote_lot_size (u64)
    pub const QUOTE_LOT_SIZE: usize = 96;
    /// min_price_multiplier (u64)
    pub const MIN_PRICE_MUL: usize = 104;
    /// max_price_multiplier (u64)
    pub const MAX_PRICE_MUL: usize = 112;
    /// system_decimal_value (u64)
    pub const SYSTEM_DEC: usize = 120;
    /// min_separate_numerator (u64)
    pub const MIN_SEP_NUM: usize = 128;
    /// min_separate_denominator (u64)
    pub const MIN_SEP_DEN: usize = 136;
    /// trade_fee_numerator (u64)
    pub const TRADE_FEE_NUM: usize = 144;
    /// trade_fee_denominator (u64)
    pub const TRADE_FEE_DEN: usize = 152;
    /// pnl_numerator (u64)
    pub const PNL_NUM: usize = 160;
    /// pnl_denominator (u64)
    pub const PNL_DEN: usize = 168;
    /// swap_fee_numerator (u64)
    pub const SWAP_FEE_NUM: usize = 176;
    /// swap_fee_denominator (u64)
    pub const SWAP_FEE_DEN: usize = 184;
    /// base_need_take_pnl (u64)
    pub const BASE_NEED_TAKE_PNL: usize = 192;
    /// quote_need_take_pnl (u64)
    pub const QUOTE_NEED_TAKE_PNL: usize = 200;
    /// quote_total_pnl (u64)
    pub const QUOTE_TOTAL_PNL: usize = 208;
    /// base_total_pnl (u64)
    pub const BASE_TOTAL_PNL: usize = 216;
    /// quote_total_deposited (u128)
    pub const QUOTE_TOTAL_DEP: usize = 224;
    /// base_total_deposited (u128)
    pub const BASE_TOTAL_DEP: usize = 240;
    /// swap_base_in_amount (u128)
    pub const SWAP_BASE_IN: usize = 256;
    /// swap_quote_out_amount (u128)
    pub const SWAP_QUOTE_OUT: usize = 272;
    /// swap_base2_quote_fee (u64)
    pub const SWAP_B2Q_FEE: usize = 288;
    /// swap_quote_in_amount (u128)
    pub const SWAP_QUOTE_IN: usize = 296;
    /// swap_base_out_amount (u128)
    pub const SWAP_BASE_OUT: usize = 312;
    /// swap_quote2_base_fee (u64)
    pub const SWAP_Q2B_FEE: usize = 328;
    /// base_vault (pubkey)
    pub const BASE_VAULT: usize = 336;
    /// quote_vault (pubkey)
    pub const QUOTE_VAULT: usize = 368;
    /// base_mint (pubkey)
    pub const BASE_MINT: usize = 400;
    /// quote_mint (pubkey)
    pub const QUOTE_MINT: usize = 432;
    /// lp_mint (pubkey)
    pub const LP_MINT: usize = 464;
    /// Minimum account size
    pub const MIN_SIZE: usize = 496;
}

// ============================================================================
// Pool Discovery Types
// ============================================================================

/// Discovered pool with pricing parameters
#[derive(Debug, Clone)]
pub struct DiscoveredPool {
    pub address: Pubkey,
    pub dex: DexType,
    pub token_mint_a: Pubkey,
    pub token_mint_b: Pubkey,
    pub token_vault_a: Pubkey,
    pub token_vault_b: Pubkey,
    /// tick_spacing for CLMM, bin_step for DLMM, 0 for AMM
    pub spacing: u16,
    /// Current tick (CLMM) or bin_id (DLMM)
    pub current_tick_or_bin: i32,
    /// sqrt_price_x64 for CLMM pools
    pub sqrt_price: Option<u128>,
    /// Active liquidity in the current tick/bin range (for slippage estimation)
    pub active_liquidity: Option<u128>,
    /// Fee in basis points (if available)
    pub fee_bps: Option<u16>,
    /// Decimals for token A
    pub decimals_a: u8,
    /// Decimals for token B
    pub decimals_b: u8,
    // === Additional accounts for swap instruction building ===
    /// AMM authority PDA (Raydium AMM)
    pub amm_authority: Option<Pubkey>,
    /// AMM config account (Raydium CLMM, PancakeSwap CLMM)
    pub amm_config: Option<Pubkey>,
    /// Observation/Oracle state (CLMM pools)
    pub observation_state: Option<Pubkey>,
    /// Tick arrays for CLMM swaps (3 for Orca, 1+ for Raydium)
    pub tick_arrays: Vec<Pubkey>,
    /// Bin array bitmap extension (DLMM)
    pub bin_array_bitmap_extension: Option<Pubkey>,
    /// Bin arrays for DLMM swaps
    pub bin_arrays: Vec<Pubkey>,
    /// Pool authority PDA (DAMM)
    pub pool_authority: Option<Pubkey>,
}

// ============================================================================
// PDA Derivation Helpers
// ============================================================================

/// Orca Whirlpool tick array size
const ORCA_TICK_ARRAY_SIZE: i32 = 88;

/// Derive Orca Whirlpool oracle PDA
pub fn derive_whirlpool_oracle(whirlpool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"oracle", whirlpool.as_ref()], &ORCA_WHIRLPOOL).0
}

/// Derive Orca Whirlpool tick array PDA
pub fn derive_whirlpool_tick_array(whirlpool: &Pubkey, start_tick_index: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"tick_array",
            whirlpool.as_ref(),
            &start_tick_index.to_le_bytes(),
        ],
        &ORCA_WHIRLPOOL,
    )
    .0
}

/// Derive 3 tick arrays for Orca Whirlpool centered on current tick
pub fn derive_whirlpool_tick_arrays(
    whirlpool: &Pubkey,
    current_tick: i32,
    tick_spacing: u16,
) -> Vec<Pubkey> {
    if tick_spacing == 0 {
        return Vec::new();
    }

    let ticks_per_array = ORCA_TICK_ARRAY_SIZE * tick_spacing as i32;
    let current_array_start = (current_tick / ticks_per_array) * ticks_per_array;

    vec![
        derive_whirlpool_tick_array(whirlpool, current_array_start),
        derive_whirlpool_tick_array(whirlpool, current_array_start - ticks_per_array),
        derive_whirlpool_tick_array(whirlpool, current_array_start + ticks_per_array),
    ]
}

/// Raydium CLMM tick array size
const RAYDIUM_TICK_ARRAY_SIZE: i32 = 60;

/// Derive Raydium CLMM tick array PDA
pub fn derive_raydium_clmm_tick_array(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"tick_array",
            pool.as_ref(),
            &start_tick_index.to_le_bytes(),
        ],
        &RAYDIUM_CLMM,
    )
    .0
}

/// Derive tick arrays for Raydium CLMM
pub fn derive_raydium_clmm_tick_arrays(
    pool: &Pubkey,
    current_tick: i32,
    tick_spacing: u16,
) -> Vec<Pubkey> {
    if tick_spacing == 0 {
        return Vec::new();
    }

    let ticks_per_array = RAYDIUM_TICK_ARRAY_SIZE * tick_spacing as i32;
    let current_array_start = (current_tick / ticks_per_array) * ticks_per_array;

    vec![
        derive_raydium_clmm_tick_array(pool, current_array_start),
        derive_raydium_clmm_tick_array(pool, current_array_start - ticks_per_array),
        derive_raydium_clmm_tick_array(pool, current_array_start + ticks_per_array),
    ]
}

/// Meteora DLMM bin array size
const DLMM_BIN_ARRAY_SIZE: i32 = 70;

/// Derive Meteora DLMM bin array PDA
pub fn derive_dlmm_bin_array(lb_pair: &Pubkey, bin_array_index: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"bin_array",
            lb_pair.as_ref(),
            &(bin_array_index as i64).to_le_bytes(),
        ],
        &METEORA_DLMM,
    )
    .0
}

/// Derive bin arrays for Meteora DLMM
pub fn derive_dlmm_bin_arrays(lb_pair: &Pubkey, active_bin: i32) -> Vec<Pubkey> {
    let bin_array_index = active_bin / DLMM_BIN_ARRAY_SIZE;

    vec![
        derive_dlmm_bin_array(lb_pair, bin_array_index),
        derive_dlmm_bin_array(lb_pair, bin_array_index - 1),
        derive_dlmm_bin_array(lb_pair, bin_array_index + 1),
    ]
}

/// Derive Meteora DAMM pool authority PDA
pub fn derive_damm_pool_authority(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"pool_authority", pool.as_ref()], &METEORA_DAMM).0
}

/// Derive Raydium AMM authority PDA
pub fn derive_raydium_amm_authority() -> Pubkey {
    // Raydium AMM uses a fixed nonce-based authority
    Pubkey::find_program_address(&[&[1, 0, 0, 0, 0][..]], &RAYDIUM_AMM).0
}

// ============================================================================
// Account Parsing
// ============================================================================

/// Parse a Whirlpool account
pub fn parse_whirlpool(address: Pubkey, data: &[u8]) -> Option<DiscoveredPool> {
    if data.len() < whirlpool_layout::MIN_SIZE {
        return None;
    }

    // Verify discriminator (Anchor: first 8 bytes)
    // Whirlpool discriminator: [63, 149, 209, 12, 225, 128, 99, 121]
    let expected_disc: [u8; 8] = [63, 149, 209, 12, 225, 128, 99, 121];
    if &data[0..8] != &expected_disc {
        return None;
    }

    let tick_spacing = u16::from_le_bytes([
        data[whirlpool_layout::TICK_SPACING],
        data[whirlpool_layout::TICK_SPACING + 1],
    ]);

    let fee_rate = u16::from_le_bytes([
        data[whirlpool_layout::FEE_RATE],
        data[whirlpool_layout::FEE_RATE + 1],
    ]);

    let sqrt_price = u128::from_le_bytes(
        data[whirlpool_layout::SQRT_PRICE..whirlpool_layout::SQRT_PRICE + 16]
            .try_into()
            .ok()?,
    );

    // Active liquidity in the current tick range
    let liquidity = u128::from_le_bytes(
        data[whirlpool_layout::LIQUIDITY..whirlpool_layout::LIQUIDITY + 16]
            .try_into()
            .ok()?,
    );

    let tick_current = i32::from_le_bytes(
        data[whirlpool_layout::TICK_CURRENT_INDEX..whirlpool_layout::TICK_CURRENT_INDEX + 4]
            .try_into()
            .ok()?,
    );

    let token_mint_a = Pubkey::try_from(
        &data[whirlpool_layout::TOKEN_MINT_A..whirlpool_layout::TOKEN_MINT_A + 32],
    )
    .ok()?;

    let token_vault_a = Pubkey::try_from(
        &data[whirlpool_layout::TOKEN_VAULT_A..whirlpool_layout::TOKEN_VAULT_A + 32],
    )
    .ok()?;

    let token_mint_b = Pubkey::try_from(
        &data[whirlpool_layout::TOKEN_MINT_B..whirlpool_layout::TOKEN_MINT_B + 32],
    )
    .ok()?;

    let token_vault_b = Pubkey::try_from(
        &data[whirlpool_layout::TOKEN_VAULT_B..whirlpool_layout::TOKEN_VAULT_B + 32],
    )
    .ok()?;

    // Derive oracle and tick arrays
    let oracle = derive_whirlpool_oracle(&address);
    let tick_arrays = derive_whirlpool_tick_arrays(&address, tick_current, tick_spacing);

    Some(DiscoveredPool {
        address,
        dex: DexType::OrcaWhirlpool,
        token_mint_a,
        token_mint_b,
        token_vault_a,
        token_vault_b,
        spacing: tick_spacing,
        current_tick_or_bin: tick_current,
        sqrt_price: Some(sqrt_price),
        active_liquidity: Some(liquidity),
        fee_bps: Some(fee_rate / 100), // fee_rate is in hundredths of a bip
        decimals_a: 0,                 // Will be fetched separately if needed
        decimals_b: 0,
        // Swap instruction accounts
        amm_authority: None,
        amm_config: None,
        observation_state: Some(oracle),
        tick_arrays,
        bin_array_bitmap_extension: None,
        bin_arrays: Vec::new(),
        pool_authority: None,
    })
}

/// Parse a Raydium CLMM PoolState account
pub fn parse_raydium_clmm(address: Pubkey, data: &[u8]) -> Option<DiscoveredPool> {
    if data.len() < raydium_clmm_layout::MIN_SIZE {
        return None;
    }

    // Raydium CLMM discriminator: [247, 237, 227, 245, 215, 195, 222, 70]
    let expected_disc: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];
    if &data[0..8] != &expected_disc {
        return None;
    }

    let tick_spacing = u16::from_le_bytes([
        data[raydium_clmm_layout::TICK_SPACING],
        data[raydium_clmm_layout::TICK_SPACING + 1],
    ]);

    let sqrt_price = u128::from_le_bytes(
        data[raydium_clmm_layout::SQRT_PRICE..raydium_clmm_layout::SQRT_PRICE + 16]
            .try_into()
            .ok()?,
    );

    // Active liquidity in the current tick range
    let liquidity = u128::from_le_bytes(
        data[raydium_clmm_layout::LIQUIDITY..raydium_clmm_layout::LIQUIDITY + 16]
            .try_into()
            .ok()?,
    );

    let tick_current = i32::from_le_bytes(
        data[raydium_clmm_layout::TICK_CURRENT..raydium_clmm_layout::TICK_CURRENT + 4]
            .try_into()
            .ok()?,
    );

    let token_mint_0 = Pubkey::try_from(
        &data[raydium_clmm_layout::TOKEN_MINT_0..raydium_clmm_layout::TOKEN_MINT_0 + 32],
    )
    .ok()?;

    let token_mint_1 = Pubkey::try_from(
        &data[raydium_clmm_layout::TOKEN_MINT_1..raydium_clmm_layout::TOKEN_MINT_1 + 32],
    )
    .ok()?;

    let token_vault_0 = Pubkey::try_from(
        &data[raydium_clmm_layout::TOKEN_VAULT_0..raydium_clmm_layout::TOKEN_VAULT_0 + 32],
    )
    .ok()?;

    let token_vault_1 = Pubkey::try_from(
        &data[raydium_clmm_layout::TOKEN_VAULT_1..raydium_clmm_layout::TOKEN_VAULT_1 + 32],
    )
    .ok()?;

    let amm_config = Pubkey::try_from(
        &data[raydium_clmm_layout::AMM_CONFIG..raydium_clmm_layout::AMM_CONFIG + 32],
    )
    .ok()?;

    let observation_key = Pubkey::try_from(
        &data[raydium_clmm_layout::OBSERVATION_KEY..raydium_clmm_layout::OBSERVATION_KEY + 32],
    )
    .ok()?;

    let decimals_0 = data[raydium_clmm_layout::DECIMALS_0];
    let decimals_1 = data[raydium_clmm_layout::DECIMALS_1];

    // Derive tick arrays
    let tick_arrays = derive_raydium_clmm_tick_arrays(&address, tick_current, tick_spacing);

    Some(DiscoveredPool {
        address,
        dex: DexType::RaydiumClmm,
        token_mint_a: token_mint_0,
        token_mint_b: token_mint_1,
        token_vault_a: token_vault_0,
        token_vault_b: token_vault_1,
        spacing: tick_spacing,
        current_tick_or_bin: tick_current,
        sqrt_price: Some(sqrt_price),
        active_liquidity: Some(liquidity),
        fee_bps: None, // Stored in AmmConfig, not pool
        decimals_a: decimals_0,
        decimals_b: decimals_1,
        // Swap instruction accounts
        amm_authority: None,
        amm_config: Some(amm_config),
        observation_state: Some(observation_key),
        tick_arrays,
        bin_array_bitmap_extension: None,
        bin_arrays: Vec::new(),
        pool_authority: None,
    })
}

/// Parse a Meteora DLMM LbPair account
pub fn parse_dlmm(address: Pubkey, data: &[u8]) -> Option<DiscoveredPool> {
    if data.len() < dlmm_layout::MIN_SIZE {
        return None;
    }

    // DLMM discriminator: [33, 11, 49, 98, 181, 101, 177, 13]
    let expected_disc: [u8; 8] = [33, 11, 49, 98, 181, 101, 177, 13];
    if &data[0..8] != &expected_disc {
        return None;
    }

    let bin_step =
        u16::from_le_bytes([data[dlmm_layout::BIN_STEP], data[dlmm_layout::BIN_STEP + 1]]);

    let active_id = i32::from_le_bytes(
        data[dlmm_layout::ACTIVE_ID..dlmm_layout::ACTIVE_ID + 4]
            .try_into()
            .ok()?,
    );

    let token_x_mint =
        Pubkey::try_from(&data[dlmm_layout::TOKEN_X_MINT..dlmm_layout::TOKEN_X_MINT + 32]).ok()?;

    let token_y_mint =
        Pubkey::try_from(&data[dlmm_layout::TOKEN_Y_MINT..dlmm_layout::TOKEN_Y_MINT + 32]).ok()?;

    let reserve_x =
        Pubkey::try_from(&data[dlmm_layout::RESERVE_X..dlmm_layout::RESERVE_X + 32]).ok()?;

    let reserve_y =
        Pubkey::try_from(&data[dlmm_layout::RESERVE_Y..dlmm_layout::RESERVE_Y + 32]).ok()?;

    // Derive bin arrays
    let bin_arrays = derive_dlmm_bin_arrays(&address, active_id);

    Some(DiscoveredPool {
        address,
        dex: DexType::MeteoraDlmm,
        token_mint_a: token_x_mint,
        token_mint_b: token_y_mint,
        token_vault_a: reserve_x,
        token_vault_b: reserve_y,
        spacing: bin_step,
        current_tick_or_bin: active_id,
        sqrt_price: None,       // DLMM uses bin_id directly
        active_liquidity: None, // DLMM liquidity is per-bin, not pooled
        fee_bps: None,          // Dynamic based on volatility
        decimals_a: 0,
        decimals_b: 0,
        // Swap instruction accounts
        amm_authority: None,
        amm_config: None,
        observation_state: None, // DLMM has oracle but different structure
        tick_arrays: Vec::new(),
        bin_array_bitmap_extension: None, // Optional, would need to check if exists
        bin_arrays,
        pool_authority: None,
    })
}

// ============================================================================
// Meteora DAMM v2 (Concentrated Liquidity with sqrt_price/liquidity)
// Reference: MeteoraAg/damm-v2/programs/cp-amm/src/state/pool.rs
// ============================================================================

/// Meteora DAMM v2 pool layout offsets
/// NOTE: Despite the name "CP-AMM", DAMM v2 uses CLMM math with sqrt_price/liquidity!
mod damm_layout {
    /// Token A mint @ 168
    pub const TOKEN_A_MINT: usize = 168;
    /// Token B mint @ 200
    pub const TOKEN_B_MINT: usize = 200;
    /// Token A vault @ 232
    pub const TOKEN_A_VAULT: usize = 232;
    /// Token B vault @ 264
    pub const TOKEN_B_VAULT: usize = 264;
    /// liquidity: u128 @ 360 (after whitelisted_vault and partner)
    pub const LIQUIDITY: usize = 360;
    /// sqrt_min_price: u128 @ 424
    pub const SQRT_MIN_PRICE: usize = 424;
    /// sqrt_max_price: u128 @ 440
    pub const SQRT_MAX_PRICE: usize = 440;
    /// sqrt_price: u128 @ 456
    pub const SQRT_PRICE: usize = 456;
    /// Minimum pool size (1104 bytes per SDK)
    pub const MIN_SIZE: usize = 1104;
}

/// Parse Meteora DAMM v2 pool account
/// IMPORTANT: DAMM v2 uses CLMM math (sqrt_price + liquidity), NOT constant product!
pub fn parse_meteora_damm(address: Pubkey, data: &[u8]) -> Option<DiscoveredPool> {
    if data.len() < damm_layout::MIN_SIZE {
        return None;
    }

    // Parse token mints and vaults
    let token_a_mint =
        Pubkey::try_from(&data[damm_layout::TOKEN_A_MINT..damm_layout::TOKEN_A_MINT + 32]).ok()?;
    let token_b_mint =
        Pubkey::try_from(&data[damm_layout::TOKEN_B_MINT..damm_layout::TOKEN_B_MINT + 32]).ok()?;
    let token_a_vault =
        Pubkey::try_from(&data[damm_layout::TOKEN_A_VAULT..damm_layout::TOKEN_A_VAULT + 32]).ok()?;
    let token_b_vault =
        Pubkey::try_from(&data[damm_layout::TOKEN_B_VAULT..damm_layout::TOKEN_B_VAULT + 32]).ok()?;

    // Parse CLMM-specific fields (critical for correct swap math!)
    let liquidity = u128::from_le_bytes(
        data[damm_layout::LIQUIDITY..damm_layout::LIQUIDITY + 16]
            .try_into()
            .ok()?,
    );

    let sqrt_price = u128::from_le_bytes(
        data[damm_layout::SQRT_PRICE..damm_layout::SQRT_PRICE + 16]
            .try_into()
            .ok()?,
    );

    // Derive current tick from sqrt_price
    // tick = 2 * log_{1.0001}(sqrt_price / 2^64)
    let current_tick = if sqrt_price > 0 {
        let sqrt_price_float = (sqrt_price as f64) / ((1u128 << 64) as f64);
        if sqrt_price_float > 0.0 {
            (2.0 * sqrt_price_float.ln() / 1.0001_f64.ln()) as i32
        } else {
            0
        }
    } else {
        0
    };

    // Derive pool authority PDA
    let pool_authority =
        Pubkey::find_program_address(&[b"pool_authority", address.as_ref()], &METEORA_DAMM).0;

    Some(DiscoveredPool {
        address,
        dex: DexType::MeteoraDamm,
        token_mint_a: token_a_mint,
        token_mint_b: token_b_mint,
        token_vault_a: token_a_vault,
        token_vault_b: token_b_vault,
        spacing: 1,                                // DAMM uses virtual ticks with spacing of 1
        current_tick_or_bin: current_tick,
        sqrt_price: Some(sqrt_price),              // CRITICAL: used for CLMM swap math
        active_liquidity: Some(liquidity),         // CRITICAL: used for CLMM swap math
        fee_bps: Some(25),                         // Default, actual may vary per pool
        decimals_a: 0,                             // Will be populated from pools.json
        decimals_b: 0,
        // Swap instruction accounts
        amm_authority: None,
        amm_config: None,
        observation_state: None,
        tick_arrays: Vec::new(),                   // DAMM doesn't use external tick arrays
        bin_array_bitmap_extension: None,
        bin_arrays: Vec::new(),
        pool_authority: Some(pool_authority),
    })
}

/// Parse a Raydium AMM (constant product) account
pub fn parse_raydium_amm(address: Pubkey, data: &[u8]) -> Option<DiscoveredPool> {
    if data.len() < raydium_amm_layout::MIN_SIZE {
        return None;
    }

    // Raydium AMM doesn't have a discriminator, check status instead
    let status = u64::from_le_bytes(
        data[raydium_amm_layout::STATUS..raydium_amm_layout::STATUS + 8]
            .try_into()
            .ok()?,
    );

    // Valid status values (from Raydium source)
    // 1 = uninitialized, 2 = initialized, 3 = disabled, etc.
    if status == 0 || status > 7 {
        return None;
    }

    let base_decimal = u64::from_le_bytes(
        data[raydium_amm_layout::BASE_DECIMAL..raydium_amm_layout::BASE_DECIMAL + 8]
            .try_into()
            .ok()?,
    );

    let quote_decimal = u64::from_le_bytes(
        data[raydium_amm_layout::QUOTE_DECIMAL..raydium_amm_layout::QUOTE_DECIMAL + 8]
            .try_into()
            .ok()?,
    );

    // Sanity check decimals
    if base_decimal > 18 || quote_decimal > 18 {
        return None;
    }

    let trade_fee_num = u64::from_le_bytes(
        data[raydium_amm_layout::TRADE_FEE_NUM..raydium_amm_layout::TRADE_FEE_NUM + 8]
            .try_into()
            .ok()?,
    );

    let trade_fee_den = u64::from_le_bytes(
        data[raydium_amm_layout::TRADE_FEE_DEN..raydium_amm_layout::TRADE_FEE_DEN + 8]
            .try_into()
            .ok()?,
    );

    let fee_bps = if trade_fee_den > 0 {
        Some(((trade_fee_num * 10000) / trade_fee_den) as u16)
    } else {
        None
    };

    let base_mint =
        Pubkey::try_from(&data[raydium_amm_layout::BASE_MINT..raydium_amm_layout::BASE_MINT + 32])
            .ok()?;

    let quote_mint = Pubkey::try_from(
        &data[raydium_amm_layout::QUOTE_MINT..raydium_amm_layout::QUOTE_MINT + 32],
    )
    .ok()?;

    let base_vault = Pubkey::try_from(
        &data[raydium_amm_layout::BASE_VAULT..raydium_amm_layout::BASE_VAULT + 32],
    )
    .ok()?;

    let quote_vault = Pubkey::try_from(
        &data[raydium_amm_layout::QUOTE_VAULT..raydium_amm_layout::QUOTE_VAULT + 32],
    )
    .ok()?;

    // Derive AMM authority
    let amm_authority = derive_raydium_amm_authority();

    Some(DiscoveredPool {
        address,
        dex: DexType::RaydiumAmm,
        token_mint_a: base_mint,
        token_mint_b: quote_mint,
        token_vault_a: base_vault,
        token_vault_b: quote_vault,
        spacing: 0, // Not applicable for constant product
        current_tick_or_bin: 0,
        sqrt_price: None,
        active_liquidity: None, // AMM uses x*y=k, liquidity = sqrt(x*y)
        fee_bps,
        decimals_a: base_decimal as u8,
        decimals_b: quote_decimal as u8,
        // Swap instruction accounts
        amm_authority: Some(amm_authority),
        amm_config: None,
        observation_state: None,
        tick_arrays: Vec::new(),
        bin_array_bitmap_extension: None,
        bin_arrays: Vec::new(),
        pool_authority: None,
    })
}

/// Parse any pool account by trying each parser
pub fn parse_pool_account(
    address: Pubkey,
    program_id: &Pubkey,
    data: &[u8],
) -> Option<DiscoveredPool> {
    if *program_id == *ORCA_WHIRLPOOL {
        parse_whirlpool(address, data)
    } else if *program_id == *RAYDIUM_CLMM {
        parse_raydium_clmm(address, data)
    } else if *program_id == *METEORA_DLMM {
        parse_dlmm(address, data)
    } else if *program_id == *METEORA_DAMM {
        parse_meteora_damm(address, data)
    } else if *program_id == *RAYDIUM_AMM {
        parse_raydium_amm(address, data)
    } else if *program_id == *PANCAKESWAP_CLMM {
        // PancakeSwap uses similar layout to Raydium CLMM
        parse_raydium_clmm(address, data).map(|mut p| {
            p.dex = DexType::PancakeSwapClmm;
            p
        })
    } else {
        None
    }
}

// ============================================================================
// Real-time Pool State Updates via Geyser
// ============================================================================

/// SPL Token Program ID for vault account subscriptions
pub const SPL_TOKEN_PROGRAM: Pubkey =
    solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// Token-2022 Program ID
pub const TOKEN_2022_PROGRAM: Pubkey =
    solana_sdk::pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

/// SPL Token Account layout offsets
mod token_account_layout {
    /// mint (pubkey)
    pub const MINT: usize = 0;
    /// owner (pubkey)
    pub const OWNER: usize = 32;
    /// amount (u64) - THE KEY VALUE
    pub const AMOUNT: usize = 64;
    /// Minimum size for a token account
    pub const MIN_SIZE: usize = 165;
}

/// Parse a token account to extract balance
pub fn parse_token_account_balance(data: &[u8]) -> Option<u64> {
    if data.len() < token_account_layout::MIN_SIZE {
        return None;
    }
    Some(u64::from_le_bytes(
        data[token_account_layout::AMOUNT..token_account_layout::AMOUNT + 8]
            .try_into()
            .ok()?,
    ))
}

// NOTE: spawn_pool_state_updater and related functions have been removed.
// All Geyser subscriptions are now consolidated in the unified_pipeline module
// via run_geyser_loop() for better connection management and reduced complexity.
