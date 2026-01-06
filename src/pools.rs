//! Pool state management
//!
//! Simple struct, two indexes, no abstractions.

use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::str::FromStr;

/// DEX type for swap instruction building
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum DexType {
    RaydiumAmm,
    RaydiumClmm,
    OrcaWhirlpool,
    MeteoraDlmm,
    MeteoraDamm,
}

/// Pool with live state
#[derive(Debug, Clone)]
pub struct Pool {
    pub address: Pubkey,
    pub dex: DexType,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub vault_a: Pubkey,
    pub vault_b: Pubkey,
    pub decimals_a: u8,
    pub decimals_b: u8,
    pub reserve_a: u64,
    pub reserve_b: u64,
    pub fee_bps: u16,
    // CLMM specific
    pub tick_spacing: Option<i32>,
    pub current_tick: Option<i32>,
    pub amm_config: Option<Pubkey>,
    pub observation_state: Option<Pubkey>,
    pub tick_arrays: Vec<TickArrayInfo>,
    // DLMM specific
    pub bin_step: Option<u16>,
    pub active_bin_id: Option<i32>,
}

/// Tick array info for CLMM pools
#[derive(Debug, Clone)]
pub struct TickArrayInfo {
    pub pubkey: Pubkey,
    pub start_tick_index: i32,
}

/// JSON format for pools file
#[derive(Debug, Deserialize)]
pub struct PoolJson {
    pub address: String,
    pub dex: DexType,
    pub token_mint_a: String,
    pub token_mint_b: String,
    pub token_vault_a: String,
    pub token_vault_b: String,
    #[serde(default = "default_decimals")]
    pub decimals_a: u8,
    #[serde(default = "default_decimals")]
    pub decimals_b: u8,
    #[serde(default = "default_fee")]
    pub fee_bps: u16,
    pub tick_spacing: Option<i32>,
    pub bin_step: Option<u16>,
}

fn default_decimals() -> u8 { 9 }
fn default_fee() -> u16 { 25 } // 0.25%

#[derive(Debug, Deserialize)]
pub struct PoolsFile {
    pub pools: Vec<PoolJson>,
}

/// Token pair key (ordered)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PairKey(pub Pubkey, pub Pubkey);

impl PairKey {
    pub fn new(a: Pubkey, b: Pubkey) -> Self {
        // Always order consistently
        if a < b { Self(a, b) } else { Self(b, a) }
    }
}

/// Which vault in a pool
#[derive(Debug, Clone, Copy)]
pub enum VaultSide {
    A,
    B,
}

/// Pool registry with indexes
pub struct PoolRegistry {
    /// All pools by address
    pub pools: HashMap<Pubkey, Pool>,
    /// vault pubkey -> (pool address, side)
    pub vault_index: HashMap<Pubkey, (Pubkey, VaultSide)>,
    /// pair -> list of pool addresses
    pub pair_index: HashMap<PairKey, Vec<Pubkey>>,
    /// All vault pubkeys (for subscription)
    pub all_vaults: Vec<Pubkey>,
    /// CLMM pool addresses (for tick tracking subscription)
    pub clmm_pools: Vec<Pubkey>,
}

impl PoolRegistry {
    /// Load from pools JSON file
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let file: PoolsFile = serde_json::from_str(&contents)?;
        
        let mut pools = HashMap::new();
        let mut vault_index = HashMap::new();
        let mut pair_index: HashMap<PairKey, Vec<Pubkey>> = HashMap::new();
        let mut all_vaults = Vec::new();
        let mut clmm_pools = Vec::new();

        for pj in file.pools {
            let address = Pubkey::from_str(&pj.address)?;
            let mint_a = Pubkey::from_str(&pj.token_mint_a)?;
            let mint_b = Pubkey::from_str(&pj.token_mint_b)?;
            let vault_a = Pubkey::from_str(&pj.token_vault_a)?;
            let vault_b = Pubkey::from_str(&pj.token_vault_b)?;

            let pool = Pool {
                address,
                dex: pj.dex,
                mint_a,
                mint_b,
                vault_a,
                vault_b,
                decimals_a: pj.decimals_a,
                decimals_b: pj.decimals_b,
                reserve_a: 0,
                reserve_b: 0,
                fee_bps: pj.fee_bps,
                tick_spacing: pj.tick_spacing,
                current_tick: None,
                amm_config: None,
                observation_state: None,
                tick_arrays: Vec::new(),
                bin_step: pj.bin_step,
                active_bin_id: None,
            };

            // Index by vault
            vault_index.insert(vault_a, (address, VaultSide::A));
            vault_index.insert(vault_b, (address, VaultSide::B));
            all_vaults.push(vault_a);
            all_vaults.push(vault_b);

            // Index by pair
            let pair = PairKey::new(mint_a, mint_b);
            pair_index.entry(pair).or_default().push(address);

            // Track CLMM pools for tick subscriptions
            if pj.dex == DexType::RaydiumClmm {
                clmm_pools.push(address);
            }

            pools.insert(address, pool);
        }

        tracing::info!(
            "Loaded {} pools, {} vaults, {} unique pairs",
            pools.len(),
            all_vaults.len(),
            pair_index.len()
        );

        Ok(Self {
            pools,
            vault_index,
            pair_index,
            all_vaults,
            clmm_pools,
        })
    }

    /// Update vault balance, return affected pool address and pair
    pub fn update_vault(&mut self, vault: &Pubkey, balance: u64) -> Option<(Pubkey, PairKey)> {
        let (pool_addr, side) = self.vault_index.get(vault)?;
        let pool = self.pools.get_mut(pool_addr)?;

        match side {
            VaultSide::A => pool.reserve_a = balance,
            VaultSide::B => pool.reserve_b = balance,
        }

        let pair = PairKey::new(pool.mint_a, pool.mint_b);
        Some((*pool_addr, pair))
    }

    /// Get all pools for a pair
    pub fn pools_for_pair(&self, pair: &PairKey) -> Vec<&Pool> {
        self.pair_index
            .get(pair)
            .map(|addrs| {
                addrs.iter()
                    .filter_map(|a| self.pools.get(a))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get pool by address
    pub fn get_pool(&self, address: &Pubkey) -> Option<&Pool> {
        self.pools.get(address)
    }

    /// Check if pool has valid reserves
    pub fn pool_has_liquidity(&self, address: &Pubkey) -> bool {
        self.pools.get(address)
            .map(|p| p.reserve_a > 0 && p.reserve_b > 0)
            .unwrap_or(false)
    }

    /// Get mutable pool reference
    pub fn get_pool_mut(&mut self, address: &Pubkey) -> Option<&mut Pool> {
        self.pools.get_mut(address)
    }

    /// Update CLMM pool current_tick from pool state data
    /// Returns Some(pair) if update succeeded, for arb checking
    pub fn update_clmm_tick(&mut self, pool_addr: &Pubkey, pool_data: &[u8]) -> Option<PairKey> {
        const POOL_TICK_CURRENT_OFFSET: usize = 253;
        
        if pool_data.len() <= POOL_TICK_CURRENT_OFFSET + 4 {
            return None;
        }
        
        let pool = self.pools.get_mut(pool_addr)?;
        
        // Only update CLMM pools
        if pool.dex != DexType::RaydiumClmm {
            return None;
        }
        
        let current_tick = i32::from_le_bytes([
            pool_data[POOL_TICK_CURRENT_OFFSET],
            pool_data[POOL_TICK_CURRENT_OFFSET + 1],
            pool_data[POOL_TICK_CURRENT_OFFSET + 2],
            pool_data[POOL_TICK_CURRENT_OFFSET + 3],
        ]);
        
        pool.current_tick = Some(current_tick);
        Some(PairKey::new(pool.mint_a, pool.mint_b))
    }

    /// Check if this pubkey is a CLMM pool address
    pub fn is_clmm_pool(&self, addr: &Pubkey) -> bool {
        self.pools.get(addr)
            .map(|p| p.dex == DexType::RaydiumClmm)
            .unwrap_or(false)
    }
}

// =============================================================================
// CLMM Tick Array Helpers
// =============================================================================

/// Raydium CLMM program ID
const RAYDIUM_CLMM: Pubkey = solana_sdk::pubkey!("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");

/// Tick array constants
const TICK_ARRAY_SIZE: i32 = 60;
const MIN_TICK: i32 = -443636;
const MAX_TICK: i32 = 443636;

/// Derive tick array PDA for a given pool and start_tick_index
pub fn derive_tick_array_pda(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    let start_bytes = start_tick_index.to_be_bytes();
    let seeds: &[&[u8]] = &[b"tick_array", pool.as_ref(), &start_bytes];
    Pubkey::find_program_address(seeds, &RAYDIUM_CLMM).0
}

/// Generate all possible tick array start indices for a given tick_spacing
pub fn all_tick_array_starts(tick_spacing: i32) -> Vec<i32> {
    let ticks_per_array = TICK_ARRAY_SIZE * tick_spacing;
    let mut starts = Vec::new();
    let mut start = (MIN_TICK / ticks_per_array) * ticks_per_array;
    while start <= MAX_TICK {
        starts.push(start);
        start += ticks_per_array;
    }
    starts
}

/// Select tick arrays needed for a swap
/// 
/// CRITICAL: The FIRST tick array must always be the one containing the current tick.
/// Subsequent arrays go in the direction of the swap:
/// - zero_for_one = true (tick DOWN): current, then descending
/// - zero_for_one = false (tick UP): current, then ascending
/// 
/// We provide 5 arrays to handle multi-tick swaps (covers ~300 ticks with spacing=60).
pub fn select_tick_arrays_for_swap(
    all_tick_arrays: &[TickArrayInfo],
    current_tick: i32,
    tick_spacing: i32,
    zero_for_one: bool,
) -> Vec<Pubkey> {
    let ticks_per_array = TICK_ARRAY_SIZE * tick_spacing;
    let current_array_start = (current_tick / ticks_per_array) * ticks_per_array;
    
    if zero_for_one {
        // Tick going DOWN - need arrays from current going down (descending order)
        // Filter: arrays at or below current, sort descending, take 5
        let mut candidates: Vec<&TickArrayInfo> = all_tick_arrays.iter()
            .filter(|ta| ta.start_tick_index <= current_array_start)
            .collect();
        candidates.sort_by(|a, b| b.start_tick_index.cmp(&a.start_tick_index));
        candidates.into_iter().map(|t| t.pubkey).take(5).collect()
    } else {
        // Tick going UP - need arrays from current going up (ascending order)
        // Filter: arrays at or above current, sort ascending, take 5
        let mut candidates: Vec<&TickArrayInfo> = all_tick_arrays.iter()
            .filter(|ta| ta.start_tick_index >= current_array_start)
            .collect();
        candidates.sort_by(|a, b| a.start_tick_index.cmp(&b.start_tick_index));
        candidates.into_iter().map(|t| t.pubkey).take(5).collect()
    }
}
