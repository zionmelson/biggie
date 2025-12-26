//! Bin Array Cache for DLMM Pools
//!
//! Caches parsed bin array data from Geyser account updates for accurate
//! Meteora DLMM swap simulation.
//!
//! With 1.5TB RAM available, we can cache bin arrays for all active pools.

use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use crate::dlmm_math::Bin;

// =============================================================================
// BIN ARRAY LAYOUT
// =============================================================================

/// Meteora DLMM bin array layout
/// Source: https://github.com/MeteoraAg/dlmm-sdk/blob/main/programs/lb_clmm/src/state/bin_array.rs
///
/// BinArray account layout:
/// - index: i64 (8 bytes) - the bin array index  
/// - version: u8 (1 byte)
/// - _padding: [u8; 7] (7 bytes)
/// - lb_pair: Pubkey (32 bytes)
/// - bins: [Bin; 70] where Bin is:
///   - amount_x: u64 (8 bytes)
///   - amount_y: u64 (8 bytes)
///   - price: u128 (16 bytes) - can be 0 if not set
///   - distribution_x: u64 (8 bytes)
///   - distribution_y: u64 (8 bytes)
///   - fee_amount_x_per_token_stored: u128 (16 bytes)
///   - fee_amount_y_per_token_stored: u128 (16 bytes)
///   - reward_per_token_stored: [u128; 2] (32 bytes)
///   Total bin size: 8 + 8 + 16 + 8 + 8 + 16 + 16 + 32 = 112 bytes
/// Header: 8 (discriminator) + 8 (index) + 1 (version) + 7 (padding) + 32 (lb_pair) = 56 bytes
const DLMM_BIN_ARRAY_HEADER: usize = 8 + 8 + 1 + 7 + 32;
const DLMM_BIN_SIZE: usize = 112;
const DLMM_BINS_PER_ARRAY: usize = 70;

// =============================================================================
// CACHED BIN INFO
// =============================================================================

/// Parsed bin data (simplified for caching)
#[derive(Debug, Clone, Copy)]
pub struct CachedBinInfo {
    /// Bin ID (computed from array index + offset)
    pub bin_id: i32,
    /// Token X amount
    pub amount_x: u64,
    /// Token Y amount
    pub amount_y: u64,
}

/// Parsed bin array data
#[derive(Debug, Clone)]
pub struct CachedBinArray {
    /// Pool this bin array belongs to
    pub pool: Pubkey,
    /// Bin array index
    pub array_index: i64,
    /// Parsed bins (only those with non-zero liquidity)
    pub bins: Vec<CachedBinInfo>,
    /// Slot when this was last updated
    pub last_update_slot: u64,
}

// =============================================================================
// BIN ARRAY PARSER
// =============================================================================

/// Parse Meteora DLMM bin array account data
pub fn parse_dlmm_bin_array(
    data: &[u8],
    pool: Pubkey,
    bin_step: u16,
) -> Option<CachedBinArray> {
    let expected_size = DLMM_BIN_ARRAY_HEADER + DLMM_BIN_SIZE * DLMM_BINS_PER_ARRAY;
    if data.len() < expected_size {
        return None;
    }

    // Parse index (offset 8, after discriminator)
    let array_index = i64::from_le_bytes(data[8..16].try_into().ok()?);

    let mut bins = Vec::new();
    let base_bin_id = (array_index * DLMM_BINS_PER_ARRAY as i64) as i32;

    for i in 0..DLMM_BINS_PER_ARRAY {
        let offset = DLMM_BIN_ARRAY_HEADER + i * DLMM_BIN_SIZE;

        // amount_x: u64 at offset 0
        let amount_x = u64::from_le_bytes(data[offset..offset + 8].try_into().ok()?);

        // amount_y: u64 at offset 8
        let amount_y = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().ok()?);

        // Only include bins with non-zero liquidity
        if amount_x > 0 || amount_y > 0 {
            let bin_id = base_bin_id + i as i32;
            bins.push(CachedBinInfo {
                bin_id,
                amount_x,
                amount_y,
            });
        }
    }

    // Suppress unused warning - will be used for price calculation
    let _ = bin_step;

    Some(CachedBinArray {
        pool,
        array_index,
        bins,
        last_update_slot: 0,
    })
}

// =============================================================================
// BIN ARRAY CACHE
// =============================================================================

/// Registration info for a bin array address
#[derive(Clone)]
struct BinArrayRegistration {
    pool: Pubkey,
    bin_step: u16,
}

/// Thread-safe cache for bin arrays
pub struct BinArrayCache {
    /// Bin array address → Parsed bin array
    arrays: RwLock<HashMap<Pubkey, CachedBinArray>>,
    /// Pool → List of bin array addresses for that pool
    pool_arrays: RwLock<HashMap<Pubkey, Vec<Pubkey>>>,
    /// Bin array address → Registration info (for parsing updates)
    registrations: RwLock<HashMap<Pubkey, BinArrayRegistration>>,
    /// Statistics
    pub arrays_cached: AtomicU64,
    pub updates_received: AtomicU64,
    pub bins_cached: AtomicU64,
}

impl BinArrayCache {
    pub fn new() -> Self {
        Self {
            arrays: RwLock::new(HashMap::new()),
            pool_arrays: RwLock::new(HashMap::new()),
            registrations: RwLock::new(HashMap::new()),
            arrays_cached: AtomicU64::new(0),
            updates_received: AtomicU64::new(0),
            bins_cached: AtomicU64::new(0),
        }
    }

    /// Register bin arrays for a pool to be tracked
    /// Call this when a pool is discovered
    pub fn register_pool(
        &self,
        pool: Pubkey,
        bin_array_addresses: &[Pubkey],
        bin_step: u16,
    ) {
        let registration = BinArrayRegistration { pool, bin_step };

        let mut registrations = self.registrations.write().unwrap();
        let mut pool_arrays = self.pool_arrays.write().unwrap();

        for addr in bin_array_addresses {
            registrations.insert(*addr, registration.clone());
        }

        pool_arrays.insert(pool, bin_array_addresses.to_vec());
    }

    /// Get all bin array addresses that need to be subscribed via Geyser
    pub fn get_all_bin_array_addresses(&self) -> Vec<Pubkey> {
        self.registrations.read().unwrap().keys().copied().collect()
    }

    /// Process an account update from Geyser
    /// Returns true if this was a registered bin array
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

        if let Some(mut cached) = parse_dlmm_bin_array(data, reg.pool, reg.bin_step) {
            cached.last_update_slot = slot;
            let bin_count = cached.bins.len() as u64;

            let mut arrays = self.arrays.write().unwrap();
            let is_new = !arrays.contains_key(address);
            arrays.insert(*address, cached);

            if is_new {
                self.arrays_cached.fetch_add(1, Ordering::Relaxed);
            }
            self.bins_cached.fetch_add(bin_count, Ordering::Relaxed);
        }

        true
    }

    /// Get all bins for a pool, merged from all cached bin arrays
    /// Returns Vec of (bin_id, amount_x, amount_y) sorted by bin_id
    pub fn get_pool_bins(&self, pool: &Pubkey) -> Vec<CachedBinInfo> {
        let pool_arrays = self.pool_arrays.read().unwrap();
        let Some(array_addrs) = pool_arrays.get(pool) else {
            return Vec::new();
        };

        let arrays = self.arrays.read().unwrap();
        let mut all_bins: Vec<CachedBinInfo> = array_addrs
            .iter()
            .filter_map(|addr| arrays.get(addr))
            .flat_map(|arr| arr.bins.iter().copied())
            .collect();

        // Sort by bin ID and deduplicate (keep last = freshest)
        all_bins.sort_by_key(|b| b.bin_id);
        all_bins.dedup_by_key(|b| b.bin_id);

        all_bins
    }

    /// Convert cached bins to Bin structs for DlmmPoolState
    pub fn get_pool_bins_for_simulation(&self, pool: &Pubkey, bin_step: u16) -> Vec<Bin> {
        self.get_pool_bins(pool)
            .into_iter()
            .filter_map(|info| Bin::new(info.bin_id, info.amount_x, info.amount_y, bin_step))
            .collect()
    }

    /// Check if we have cached bin data for a pool
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
            self.bins_cached.load(Ordering::Relaxed),
        )
    }
}

impl Default for BinArrayCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bin_array_cache_registration() {
        let cache = BinArrayCache::new();
        let pool = Pubkey::new_unique();
        let arrays = vec![Pubkey::new_unique(), Pubkey::new_unique()];

        cache.register_pool(pool, &arrays, 25);

        let all_addrs = cache.get_all_bin_array_addresses();
        assert_eq!(all_addrs.len(), 2);
        assert!(all_addrs.contains(&arrays[0]));
        assert!(all_addrs.contains(&arrays[1]));
    }

    #[test]
    fn test_get_pool_bins_empty() {
        let cache = BinArrayCache::new();
        let pool = Pubkey::new_unique();

        let bins = cache.get_pool_bins(&pool);
        assert!(bins.is_empty());
    }
}
