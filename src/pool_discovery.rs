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

use crate::amm_math::{CurveType, PoolStateCache};
use crate::geyser::{GeyserClient, GeyserConfig};
use crate::shredstream::programs::{
    DexType, METEORA_DAMM, METEORA_DLMM, ORCA_WHIRLPOOL, PANCAKESWAP_CLMM, RAYDIUM_AMM, RAYDIUM_CLMM,
};
use anyhow::{Context, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

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
    /// Fee in basis points (if available)
    pub fee_bps: Option<u16>,
    /// Decimals for token A
    pub decimals_a: u8,
    /// Decimals for token B
    pub decimals_b: u8,
}

/// Dynamic pool registry built from Geyser
pub struct DynamicPoolRegistry {
    /// Pool address → discovered pool info
    pools: RwLock<HashMap<Pubkey, DiscoveredPool>>,
    /// Token pair → pool addresses (for routing)
    pair_index: RwLock<HashMap<(Pubkey, Pubkey), Vec<Pubkey>>>,
    /// Statistics
    pub stats: PoolDiscoveryStats,
}

#[derive(Debug, Default)]
pub struct PoolDiscoveryStats {
    pub whirlpools_discovered: AtomicU64,
    pub raydium_clmm_discovered: AtomicU64,
    pub raydium_amm_discovered: AtomicU64,
    pub dlmm_discovered: AtomicU64,
    pub pancake_discovered: AtomicU64,
    pub parse_errors: AtomicU64,
    pub last_update_slot: AtomicU64,
}

impl DynamicPoolRegistry {
    pub fn new() -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            pair_index: RwLock::new(HashMap::new()),
            stats: PoolDiscoveryStats::default(),
        }
    }

    /// Get a pool by address
    pub fn get(&self, address: &Pubkey) -> Option<DiscoveredPool> {
        self.pools.read().unwrap().get(address).cloned()
    }

    /// Get pools for a token pair
    pub fn get_pools_for_pair(&self, mint_a: &Pubkey, mint_b: &Pubkey) -> Vec<Pubkey> {
        let index = self.pair_index.read().unwrap();
        // Try both orderings
        if let Some(pools) = index.get(&(*mint_a, *mint_b)) {
            return pools.clone();
        }
        if let Some(pools) = index.get(&(*mint_b, *mint_a)) {
            return pools.clone();
        }
        Vec::new()
    }

    /// Register a discovered pool
    pub fn register(&self, pool: DiscoveredPool) {
        let address = pool.address;
        let mint_a = pool.token_mint_a;
        let mint_b = pool.token_mint_b;
        let dex = pool.dex;

        // Update pools map
        {
            let mut pools = self.pools.write().unwrap();
            pools.insert(address, pool);
        }

        // Update pair index
        {
            let mut index = self.pair_index.write().unwrap();
            // Index in canonical order (lower pubkey first)
            let key = if mint_a < mint_b {
                (mint_a, mint_b)
            } else {
                (mint_b, mint_a)
            };
            index.entry(key).or_insert_with(Vec::new).push(address);
        }

        // Update stats
        match dex {
            DexType::OrcaWhirlpool => self.stats.whirlpools_discovered.fetch_add(1, Ordering::Relaxed),
            DexType::RaydiumClmm => self.stats.raydium_clmm_discovered.fetch_add(1, Ordering::Relaxed),
            DexType::RaydiumAmm => self.stats.raydium_amm_discovered.fetch_add(1, Ordering::Relaxed),
            DexType::MeteoraDlmm => self.stats.dlmm_discovered.fetch_add(1, Ordering::Relaxed),
            DexType::PancakeSwapClmm => self.stats.pancake_discovered.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }

    /// Total pools discovered
    pub fn len(&self) -> usize {
        self.pools.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.pools.read().unwrap().is_empty()
    }

    /// Get spacing (tick_spacing or bin_step) for pricing calculations
    pub fn get_spacing(&self, pool_address: &Pubkey) -> Option<u16> {
        self.pools.read().unwrap().get(pool_address).map(|p| p.spacing)
    }

    /// Get all pools (for syncing to evaluator)
    pub fn all_pools(&self) -> Vec<DiscoveredPool> {
        self.pools.read().unwrap().values().cloned().collect()
    }
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
        fee_bps: Some(fee_rate / 100), // fee_rate is in hundredths of a bip
        decimals_a: 0, // Will be fetched separately if needed
        decimals_b: 0,
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

    let decimals_0 = data[raydium_clmm_layout::DECIMALS_0];
    let decimals_1 = data[raydium_clmm_layout::DECIMALS_1];

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
        fee_bps: None, // Stored in AmmConfig, not pool
        decimals_a: decimals_0,
        decimals_b: decimals_1,
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

    let bin_step = u16::from_le_bytes([
        data[dlmm_layout::BIN_STEP],
        data[dlmm_layout::BIN_STEP + 1],
    ]);

    let active_id = i32::from_le_bytes(
        data[dlmm_layout::ACTIVE_ID..dlmm_layout::ACTIVE_ID + 4]
            .try_into()
            .ok()?,
    );

    let token_x_mint = Pubkey::try_from(
        &data[dlmm_layout::TOKEN_X_MINT..dlmm_layout::TOKEN_X_MINT + 32],
    )
    .ok()?;

    let token_y_mint = Pubkey::try_from(
        &data[dlmm_layout::TOKEN_Y_MINT..dlmm_layout::TOKEN_Y_MINT + 32],
    )
    .ok()?;

    let reserve_x = Pubkey::try_from(
        &data[dlmm_layout::RESERVE_X..dlmm_layout::RESERVE_X + 32],
    )
    .ok()?;

    let reserve_y = Pubkey::try_from(
        &data[dlmm_layout::RESERVE_Y..dlmm_layout::RESERVE_Y + 32],
    )
    .ok()?;

    Some(DiscoveredPool {
        address,
        dex: DexType::MeteoraDlmm,
        token_mint_a: token_x_mint,
        token_mint_b: token_y_mint,
        token_vault_a: reserve_x,
        token_vault_b: reserve_y,
        spacing: bin_step,
        current_tick_or_bin: active_id,
        sqrt_price: None, // DLMM uses bin_id directly
        fee_bps: None,    // Dynamic based on volatility
        decimals_a: 0,
        decimals_b: 0,
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

    let base_mint = Pubkey::try_from(
        &data[raydium_amm_layout::BASE_MINT..raydium_amm_layout::BASE_MINT + 32],
    )
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
        fee_bps,
        decimals_a: base_decimal as u8,
        decimals_b: quote_decimal as u8,
    })
}

/// Parse any pool account by trying each parser
pub fn parse_pool_account(address: Pubkey, program_id: &Pubkey, data: &[u8]) -> Option<DiscoveredPool> {
    if *program_id == *ORCA_WHIRLPOOL {
        parse_whirlpool(address, data)
    } else if *program_id == *RAYDIUM_CLMM {
        parse_raydium_clmm(address, data)
    } else if *program_id == *METEORA_DLMM {
        parse_dlmm(address, data)
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
// Pool Discovery Service
// ============================================================================

/// Pool discovery service using Geyser
pub struct PoolDiscoveryService {
    registry: Arc<DynamicPoolRegistry>,
    rpc_client: Arc<RpcClient>,
}

impl PoolDiscoveryService {
    pub fn new(rpc_client: Arc<RpcClient>) -> Self {
        Self {
            registry: Arc::new(DynamicPoolRegistry::new()),
            rpc_client,
        }
    }

    pub fn registry(&self) -> Arc<DynamicPoolRegistry> {
        Arc::clone(&self.registry)
    }

    /// Bootstrap: fetch all existing pools from RPC (one-time at startup)
    pub async fn bootstrap(&self) -> Result<()> {
        tracing::info!("Bootstrapping pool discovery from RPC...");

        // Fetch pools from each program
        let programs = [
            (*ORCA_WHIRLPOOL, "Whirlpool"),
            (*RAYDIUM_CLMM, "Raydium CLMM"),
            (*METEORA_DLMM, "DLMM"),
            // Skip Raydium AMM for now - too many accounts
        ];

        for (program_id, name) in programs {
            match self.fetch_program_accounts(&program_id).await {
                Ok(count) => {
                    tracing::info!("Discovered {} {} pools", count, name);
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch {} accounts: {}", name, e);
                }
            }
        }

        tracing::info!(
            "Pool discovery bootstrap complete: {} total pools",
            self.registry.len()
        );

        Ok(())
    }

    /// Fetch and parse all accounts for a program
    async fn fetch_program_accounts(&self, program_id: &Pubkey) -> Result<usize> {
        // Use getProgramAccounts - the client will return decoded account data
        // Note: For large programs like Raydium AMM, this can be slow
        // Consider using filters for specific account sizes/discriminators
        
        let accounts = self
            .rpc_client
            .get_program_accounts(program_id)
            .await
            .context("Failed to fetch program accounts")?;

        let mut count = 0;
        for (address, account) in accounts {
            if let Some(pool) = parse_pool_account(address, program_id, &account.data) {
                self.registry.register(pool);
                count += 1;
            }
        }

        Ok(count)
    }

    /// Subscribe to program account updates via Geyser (continuous)
    pub async fn subscribe_updates(&self, geyser_config: GeyserConfig) -> Result<()> {
        tracing::info!("Starting Geyser pool account subscription...");

        let client = GeyserClient::new(geyser_config)
            .await
            .context("Failed to connect to Geyser for pool discovery")?;

        // Subscribe to accounts for each DEX program
        // Note: This requires implementing SubscribeAccountUpdates in geyser.rs
        // For now, we'll rely on bootstrap + transaction parsing

        tracing::info!("Pool subscription ready (using transaction-based discovery)");
        Ok(())
    }

    /// Update registry from a transaction's accounts (called during pipeline processing)
    pub fn discover_from_transaction_accounts(
        &self,
        accounts: &[(Pubkey, &[u8])],
        program_ids: &HashMap<Pubkey, DexType>,
    ) {
        for (address, data) in accounts {
            for (program_id, _) in program_ids {
                if let Some(pool) = parse_pool_account(*address, program_id, data) {
                    // Only register if we don't already have it
                    if self.registry.get(address).is_none() {
                        self.registry.register(pool);
                    }
                    break;
                }
            }
        }
    }
}

// ============================================================================
// Integration with PoolStateCache
// ============================================================================

impl DynamicPoolRegistry {
    /// Sync discovered pools to PoolStateCache for swap calculations
    pub fn sync_to_pool_state_cache(&self, cache: &PoolStateCache, slot: u64) {
        let pools = self.pools.read().unwrap();
        
        for (_, pool) in pools.iter() {
            let curve = match pool.dex {
                DexType::RaydiumAmm | DexType::MeteoraDamm => CurveType::ConstantProduct,
                _ => CurveType::ConcentratedLiquidity,
            };
            
            // Default fee estimates by DEX (will be updated from trades)
            let fee_bps = pool.fee_bps.unwrap_or(match pool.dex {
                DexType::RaydiumAmm => 25, // 0.25%
                DexType::RaydiumClmm => 25,
                DexType::OrcaWhirlpool => 30, // 0.3%
                DexType::MeteoraDlmm => 10, // Dynamic, but base ~0.1%
                DexType::MeteoraDamm => 25,
                DexType::PancakeSwapClmm => 25,
            });
            
            cache.update(
                &pool.address.to_string(),
                &pool.token_mint_a.to_string(),
                &pool.token_mint_b.to_string(),
                0, // base_reserve - will be updated from account data
                0, // quote_reserve - will be updated from account data
                pool.decimals_a,
                pool.decimals_b,
                curve,
                fee_bps,
                slot,
            );
            
            // For CLMM pools, also set canonical price data if available
            if pool.spacing > 0 {
                if let Some(sqrt_price) = pool.sqrt_price {
                    // Convert sqrt_price_x64 to price
                    let sqrt = (sqrt_price as f64) / ((1u128 << 64) as f64);
                    let price = sqrt * sqrt;
                    
                    cache.update_canonical_price(
                        &pool.address.to_string(),
                        price,
                        pool.current_tick_or_bin,
                        pool.spacing,
                    );
                }
            }
        }
    }
}

// ============================================================================
// Real-time Pool State Updates via Geyser
// ============================================================================

/// Spawn a background task that subscribes to program account updates via Geyser
/// and updates the pool registry and state cache in real-time.
pub async fn spawn_pool_state_updater(
    geyser_config: GeyserConfig,
    registry: Arc<DynamicPoolRegistry>,
    pool_states: Arc<PoolStateCache>,
) -> Result<()> {
    tokio::spawn(async move {
        loop {
            match run_pool_state_updater(&geyser_config, &registry, &pool_states).await {
                Ok(()) => {
                    tracing::info!("Pool state updater ended normally");
                    break;
                }
                Err(e) => {
                    tracing::warn!("Pool state updater error: {}, reconnecting in 3s...", e);
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    });
    Ok(())
}

async fn run_pool_state_updater(
    config: &GeyserConfig,
    registry: &Arc<DynamicPoolRegistry>,
    pool_states: &Arc<PoolStateCache>,
) -> Result<()> {
    let client = GeyserClient::new(config.clone()).await?;
    
    // Subscribe to all DEX program account updates
    let programs = vec![
        *ORCA_WHIRLPOOL,
        *RAYDIUM_CLMM,
        *METEORA_DLMM,
        // *RAYDIUM_AMM, // Too many accounts - optional
    ];
    
    let mut stream = client
        .subscribe_programs(programs)
        .await
        .context("Failed to subscribe to program updates")?;
    
    tracing::info!("Geyser pool state updater connected");
    
    while let Some(timestamped_update) = stream.message().await? {
        let Some(account_update) = timestamped_update.account_update else {
            continue;
        };
        
        let slot = account_update.slot;
        registry.stats.last_update_slot.store(slot, Ordering::Relaxed);
        
        // Parse pubkey
        if account_update.pubkey.len() != 32 {
            continue;
        }
        let mut pubkey_bytes = [0u8; 32];
        pubkey_bytes.copy_from_slice(&account_update.pubkey);
        let address = Pubkey::new_from_array(pubkey_bytes);
        
        // Parse owner (program_id)
        if account_update.owner.len() != 32 {
            continue;
        }
        let mut owner_bytes = [0u8; 32];
        owner_bytes.copy_from_slice(&account_update.owner);
        let program_id = Pubkey::new_from_array(owner_bytes);
        
        // Try to parse as a pool account
        if let Some(pool) = parse_pool_account(address, &program_id, &account_update.data) {
            // Update registry
            registry.register(pool.clone());
            
            // Update pool state cache with price info
            let curve = match pool.dex {
                DexType::RaydiumAmm | DexType::MeteoraDamm => CurveType::ConstantProduct,
                _ => CurveType::ConcentratedLiquidity,
            };
            
            let fee_bps = pool.fee_bps.unwrap_or(25);
            
            pool_states.update(
                &pool.address.to_string(),
                &pool.token_mint_a.to_string(),
                &pool.token_mint_b.to_string(),
                0, 0, // Reserves updated separately from vault accounts
                pool.decimals_a,
                pool.decimals_b,
                curve,
                fee_bps,
                slot,
            );
            
            // Update CLMM price if available
            if pool.spacing > 0 {
                if let Some(sqrt_price) = pool.sqrt_price {
                    let sqrt = (sqrt_price as f64) / ((1u128 << 64) as f64);
                    let price = sqrt * sqrt;
                    
                    pool_states.update_canonical_price(
                        &pool.address.to_string(),
                        price,
                        pool.current_tick_or_bin,
                        pool.spacing,
                    );
                }
            }
            
            tracing::trace!(
                "POOL UPDATE: {} {:?} tick={} sqrt_price={:?}",
                &pool.address.to_string()[..8],
                pool.dex,
                pool.current_tick_or_bin,
                pool.sqrt_price
            );
        }
    }
    
    Ok(())
}
