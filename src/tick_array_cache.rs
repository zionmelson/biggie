//! Tick Array Cache for CLMM Pools
//!
//! Caches parsed tick array data from Geyser account updates for accurate
//! CLMM swap simulation. Supports both Orca Whirlpool and Raydium CLMM layouts.
//!
//! With 1.5TB RAM available, we can cache tick arrays for all active pools.

use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use crate::clmm_math::TickInfo;

// =============================================================================
// TICK ARRAY LAYOUTS
// =============================================================================

/// Orca Whirlpool tick array layout
/// Source: https://github.com/orca-so/whirlpools/blob/main/programs/whirlpool/src/state/tick.rs
///
/// TickArray account layout:
/// - start_tick_index: i32 (4 bytes)
/// - ticks: [Tick; 88] where Tick is:
///   - initialized: bool (1 byte)
///   - liquidity_net: i128 (16 bytes)
///   - liquidity_gross: u128 (16 bytes)
///   - fee_growth_outside_a: u128 (16 bytes)
///   - fee_growth_outside_b: u128 (16 bytes)
///   - reward_growths_outside: [u128; 3] (48 bytes)
/// Total tick size: 1 + 16 + 16 + 16 + 16 + 48 = 113 bytes
/// Array header: 8 bytes (discriminator) + 4 bytes (start_tick_index) + 32 bytes (whirlpool pubkey)
const ORCA_TICK_ARRAY_HEADER: usize = 8 + 4 + 32; // discriminator + start_tick_index + whirlpool
const ORCA_TICK_SIZE: usize = 113;
const ORCA_TICKS_PER_ARRAY: usize = 88;

/// Raydium CLMM tick array layout
/// Source: https://github.com/raydium-io/raydium-clmm/blob/master/programs/amm/src/states/tick_array.rs
///
/// TickArrayState layout:
/// - pool_id: Pubkey (32 bytes)
/// - start_tick_index: i32 (4 bytes)
/// - ticks: [TickState; 60] where TickState is:
///   - tick: i32 (4 bytes) - implicit from position
///   - liquidity_gross: u128 (16 bytes)
///   - liquidity_net: i128 (16 bytes)
///   - fee_growth_outside_0_x64: u128 (16 bytes)
///   - fee_growth_outside_1_x64: u128 (16 bytes)
///   - reward_growths_outside_x64: [u128; 3] (48 bytes)
///   - padding: [u32; 13] (52 bytes)
/// Total tick size: 16 + 16 + 16 + 16 + 48 + 52 = 164 bytes
/// Header: 8 (discriminator) + 32 (pool_id) + 4 (start_tick_index) + 4 (initialized_tick_count) = 48 bytes
const RAYDIUM_TICK_ARRAY_HEADER: usize = 8 + 32 + 4 + 4;
const RAYDIUM_TICK_SIZE: usize = 164;
const RAYDIUM_TICKS_PER_ARRAY: usize = 60;

// =============================================================================
// CACHED TICK ARRAY
// =============================================================================

/// Parsed tick array data
#[derive(Debug, Clone)]
pub struct CachedTickArray {
    /// Pool this tick array belongs to
    pub pool: Pubkey,
    /// Start tick index of this array
    pub start_tick_index: i32,
    /// Tick spacing (needed to compute actual tick indices)
    pub tick_spacing: u16,
    /// Parsed ticks (only initialized ones with non-zero liquidity_net)
    pub ticks: Vec<TickInfo>,
    /// Slot when this was last updated
    pub last_update_slot: u64,
}

/// Pool type for parsing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClmmPoolType {
    OrcaWhirlpool,
    RaydiumClmm,
}

// =============================================================================
// TICK ARRAY PARSER
// =============================================================================

/// Parse Orca Whirlpool tick array account data
pub fn parse_orca_tick_array(
    data: &[u8],
    pool: Pubkey,
    tick_spacing: u16,
) -> Option<CachedTickArray> {
    if data.len() < ORCA_TICK_ARRAY_HEADER + ORCA_TICK_SIZE * ORCA_TICKS_PER_ARRAY {
        return None;
    }

    // Parse start_tick_index (offset 8, after discriminator)
    let start_tick_index = i32::from_le_bytes(data[8..12].try_into().ok()?);

    let mut ticks = Vec::new();

    for i in 0..ORCA_TICKS_PER_ARRAY {
        let offset = ORCA_TICK_ARRAY_HEADER + i * ORCA_TICK_SIZE;

        // initialized: bool at offset 0
        let initialized = data[offset] != 0;

        if initialized {
            // liquidity_net: i128 at offset 1
            let liquidity_net =
                i128::from_le_bytes(data[offset + 1..offset + 17].try_into().ok()?);

            // Only include ticks with non-zero liquidity_net (they affect swaps)
            if liquidity_net != 0 {
                let tick = start_tick_index + (i as i32) * (tick_spacing as i32);
                ticks.push(TickInfo {
                    tick,
                    liquidity_net,
                    initialized: true,
                });
            }
        }
    }

    Some(CachedTickArray {
        pool,
        start_tick_index,
        tick_spacing,
        ticks,
        last_update_slot: 0,
    })
}

/// Parse Raydium CLMM tick array account data
pub fn parse_raydium_tick_array(
    data: &[u8],
    pool: Pubkey,
    tick_spacing: u16,
) -> Option<CachedTickArray> {
    if data.len() < RAYDIUM_TICK_ARRAY_HEADER + RAYDIUM_TICK_SIZE * RAYDIUM_TICKS_PER_ARRAY {
        return None;
    }

    // Parse start_tick_index (offset 8 + 32 = 40, after discriminator + pool_id)
    let start_tick_index = i32::from_le_bytes(data[40..44].try_into().ok()?);

    let mut ticks = Vec::new();

    for i in 0..RAYDIUM_TICKS_PER_ARRAY {
        let offset = RAYDIUM_TICK_ARRAY_HEADER + i * RAYDIUM_TICK_SIZE;

        // liquidity_gross: u128 at offset 0
        let liquidity_gross =
            u128::from_le_bytes(data[offset..offset + 16].try_into().ok()?);

        // Only process ticks with non-zero liquidity (initialized)
        if liquidity_gross > 0 {
            // liquidity_net: i128 at offset 16
            let liquidity_net =
                i128::from_le_bytes(data[offset + 16..offset + 32].try_into().ok()?);

            let tick = start_tick_index + (i as i32) * (tick_spacing as i32);
            ticks.push(TickInfo {
                tick,
                liquidity_net,
                initialized: true,
            });
        }
    }

    Some(CachedTickArray {
        pool,
        start_tick_index,
        tick_spacing,
        ticks,
        last_update_slot: 0,
    })
}

// =============================================================================
// TICK ARRAY CACHE
// =============================================================================

/// Registration info for a tick array address
#[derive(Clone)]
struct TickArrayRegistration {
    pool: Pubkey,
    tick_spacing: u16,
    pool_type: ClmmPoolType,
}

/// Thread-safe cache for tick arrays
pub struct TickArrayCache {
    /// Tick array address → Parsed tick array
    arrays: RwLock<HashMap<Pubkey, CachedTickArray>>,
    /// Pool → List of tick array addresses for that pool
    pool_arrays: RwLock<HashMap<Pubkey, Vec<Pubkey>>>,
    /// Tick array address → Registration info (for parsing updates)
    registrations: RwLock<HashMap<Pubkey, TickArrayRegistration>>,
    /// Statistics
    pub arrays_cached: AtomicU64,
    pub updates_received: AtomicU64,
    pub ticks_cached: AtomicU64,
}

impl TickArrayCache {
    pub fn new() -> Self {
        Self {
            arrays: RwLock::new(HashMap::new()),
            pool_arrays: RwLock::new(HashMap::new()),
            registrations: RwLock::new(HashMap::new()),
            arrays_cached: AtomicU64::new(0),
            updates_received: AtomicU64::new(0),
            ticks_cached: AtomicU64::new(0),
        }
    }

    /// Register tick arrays for a pool to be tracked
    /// Call this when a pool is discovered
    pub fn register_pool(
        &self,
        pool: Pubkey,
        tick_array_addresses: &[Pubkey],
        tick_spacing: u16,
        pool_type: ClmmPoolType,
    ) {
        let registration = TickArrayRegistration {
            pool,
            tick_spacing,
            pool_type,
        };

        let mut registrations = self.registrations.write().unwrap();
        let mut pool_arrays = self.pool_arrays.write().unwrap();

        for addr in tick_array_addresses {
            registrations.insert(*addr, registration.clone());
        }

        pool_arrays.insert(pool, tick_array_addresses.to_vec());
    }

    /// Get all tick array addresses that need to be subscribed via Geyser
    pub fn get_all_tick_array_addresses(&self) -> Vec<Pubkey> {
        self.registrations.read().unwrap().keys().copied().collect()
    }

    /// Process an account update from Geyser
    /// Returns true if this was a registered tick array
    pub fn process_account_update(
        &self,
        address: &Pubkey,
        data: &[u8],
        slot: u64,
    ) -> bool {
        let registration = {
            let registrations = self.registrations.read().unwrap();
            registrations.get(address).cloned()
        };

        let Some(reg) = registration else {
            return false;
        };

        self.updates_received.fetch_add(1, Ordering::Relaxed);

        // Parse based on pool type
        let parsed = match reg.pool_type {
            ClmmPoolType::OrcaWhirlpool => {
                parse_orca_tick_array(data, reg.pool, reg.tick_spacing)
            }
            ClmmPoolType::RaydiumClmm => {
                parse_raydium_tick_array(data, reg.pool, reg.tick_spacing)
            }
        };

        if let Some(mut cached) = parsed {
            cached.last_update_slot = slot;
            let tick_count = cached.ticks.len() as u64;

            let mut arrays = self.arrays.write().unwrap();
            let is_new = !arrays.contains_key(address);
            arrays.insert(*address, cached);

            if is_new {
                self.arrays_cached.fetch_add(1, Ordering::Relaxed);
            }
            self.ticks_cached.fetch_add(tick_count, Ordering::Relaxed);
        }

        true
    }

    /// Get all ticks for a pool, merged from all cached tick arrays
    /// Ticks are sorted by tick index
    pub fn get_pool_ticks(&self, pool: &Pubkey) -> Vec<TickInfo> {
        let pool_arrays = self.pool_arrays.read().unwrap();
        let Some(array_addrs) = pool_arrays.get(pool) else {
            return Vec::new();
        };

        let arrays = self.arrays.read().unwrap();
        let mut all_ticks: Vec<TickInfo> = array_addrs
            .iter()
            .filter_map(|addr| arrays.get(addr))
            .flat_map(|arr| arr.ticks.iter().copied())
            .collect();

        // Sort by tick index and deduplicate
        all_ticks.sort_by_key(|t| t.tick);
        all_ticks.dedup_by_key(|t| t.tick);

        all_ticks
    }

    /// Check if we have cached tick data for a pool
    pub fn has_pool_data(&self, pool: &Pubkey) -> bool {
        let pool_arrays = self.pool_arrays.read().unwrap();
        let Some(array_addrs) = pool_arrays.get(pool) else {
            return false;
        };

        let arrays = self.arrays.read().unwrap();
        array_addrs.iter().any(|addr| arrays.contains_key(addr))
    }

    /// Get stats for logging
    pub fn stats(&self) -> (u64, u64, u64) {
        (
            self.arrays_cached.load(Ordering::Relaxed),
            self.updates_received.load(Ordering::Relaxed),
            self.ticks_cached.load(Ordering::Relaxed),
        )
    }
}

impl Default for TickArrayCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_array_cache_registration() {
        let cache = TickArrayCache::new();
        let pool = Pubkey::new_unique();
        let arrays = vec![Pubkey::new_unique(), Pubkey::new_unique()];

        cache.register_pool(pool, &arrays, 64, ClmmPoolType::OrcaWhirlpool);

        let all_addrs = cache.get_all_tick_array_addresses();
        assert_eq!(all_addrs.len(), 2);
        assert!(all_addrs.contains(&arrays[0]));
        assert!(all_addrs.contains(&arrays[1]));
    }

    #[test]
    fn test_get_pool_ticks_empty() {
        let cache = TickArrayCache::new();
        let pool = Pubkey::new_unique();

        let ticks = cache.get_pool_ticks(&pool);
        assert!(ticks.is_empty());
    }
}
