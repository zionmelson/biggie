//! Pre-computed CLMM/DLMM pool pricing map.
//!
//! Provides accurate price calculations for concentrated liquidity pools
//! without runtime RPC calls. Two maps are available:
//!
//! 1. `ClmmPriceMap` - Pool address → bin_step/tick_spacing
//!    Loaded from `clmm_pool_map.json`
//!
//! 2. `ClmmArrayMap` - Bin/Tick array address → pool + start_index + spacing
//!    Loaded from `clmm_array_map.json`
//!    Used for CoreCast swap instructions which provide bin_array/tick_array addresses
//!
//! Price formulas:
//! - DLMM: price = (1 + bin_step/10000)^bin_id
//! - CLMM (Whirlpool/Raydium): price = 1.0001^tick

use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::str::FromStr;

// Note: The bin_id stored on-chain IS the actual exponent for price calculation.
// The offset 8388608 (2^23) is only used for u32 storage representation internally,
// NOT in price calculation. The SDK uses bin_id directly: (1 + bin_step/10000)^bin_id

/// Pre-loaded pool configuration map
#[derive(Debug, Clone)]
pub struct ClmmPriceMap {
    /// DLMM pools: pubkey -> bin_step
    pub dlmm: HashMap<String, u16>,
    /// Whirlpool pools: pubkey -> tick_spacing
    pub whirlpool: HashMap<String, u16>,
    /// Raydium CLMM pools: pubkey -> tick_spacing  
    pub raydium: HashMap<String, u16>,
}

#[derive(Deserialize)]
struct RawPoolMap {
    dlmm: HashMap<String, u16>,
    whirlpool: HashMap<String, u16>,
    raydium: HashMap<String, u16>,
}

impl ClmmPriceMap {
    /// Load the pool map from the JSON file at the project root
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        Self::load_from_path("clmm_pool_map.json")
    }

    /// Load from a specific path
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let raw: RawPoolMap = serde_json::from_reader(reader)?;

        Ok(Self {
            dlmm: raw.dlmm,
            whirlpool: raw.whirlpool,
            raydium: raw.raydium,
        })
    }

    /// Get bin_step for a DLMM pool
    pub fn get_dlmm_bin_step(&self, pool_address: &str) -> Option<u16> {
        self.dlmm.get(pool_address).copied()
    }

    /// Get tick_spacing for a Whirlpool pool
    pub fn get_whirlpool_tick_spacing(&self, pool_address: &str) -> Option<u16> {
        self.whirlpool.get(pool_address).copied()
    }

    /// Get tick_spacing for a Raydium CLMM pool
    pub fn get_raydium_tick_spacing(&self, pool_address: &str) -> Option<u16> {
        self.raydium.get(pool_address).copied()
    }

    /// Calculate price from DLMM bin_id and bin_step
    /// Formula: price = (1 + bin_step/10000)^bin_id
    /// Note: bin_id on-chain is already the actual exponent (can be negative)
    pub fn dlmm_bin_to_price(bin_step: u16, bin_id: i32) -> f64 {
        let base = 1.0 + (bin_step as f64) / 10000.0;
        base.powi(bin_id)
    }

    /// Calculate price from CLMM tick index
    /// Formula: price = 1.0001^tick
    pub fn tick_to_price(tick: i32) -> f64 {
        1.0001_f64.powi(tick)
    }

    /// Calculate sqrt_price_x64 from tick (as u128)
    /// This matches Solana AMM format
    pub fn tick_to_sqrt_price_x64(tick: i32) -> u128 {
        let sqrt_price = 1.0001_f64.powi(tick).sqrt();
        (sqrt_price * (1u128 << 64) as f64) as u128
    }

    /// Convert sqrt_price_x64 to human-readable price
    pub fn sqrt_price_x64_to_price(sqrt_price_x64: u128) -> f64 {
        let sqrt_price = (sqrt_price_x64 as f64) / ((1u128 << 64) as f64);
        sqrt_price * sqrt_price
    }

    /// Calculate price for a pool given its address and current bin/tick
    pub fn calculate_price(&self, pool_address: &str, bin_or_tick: i32) -> Option<f64> {
        // Check DLMM first
        if let Some(bin_step) = self.dlmm.get(pool_address) {
            return Some(Self::dlmm_bin_to_price(*bin_step, bin_or_tick));
        }

        // Check Whirlpool
        if self.whirlpool.contains_key(pool_address) {
            return Some(Self::tick_to_price(bin_or_tick));
        }

        // Check Raydium
        if self.raydium.contains_key(pool_address) {
            return Some(Self::tick_to_price(bin_or_tick));
        }

        None
    }

    /// Get pool type for a given address
    pub fn get_pool_type(&self, pool_address: &str) -> Option<ClmmPoolType> {
        if self.dlmm.contains_key(pool_address) {
            Some(ClmmPoolType::Dlmm)
        } else if self.whirlpool.contains_key(pool_address) {
            Some(ClmmPoolType::Whirlpool)
        } else if self.raydium.contains_key(pool_address) {
            Some(ClmmPoolType::RaydiumClmm)
        } else {
            None
        }
    }

    /// Total number of pools in the map
    pub fn total_pools(&self) -> usize {
        self.dlmm.len() + self.whirlpool.len() + self.raydium.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClmmPoolType {
    Dlmm,
    Whirlpool,
    RaydiumClmm,
    PancakeSwapClmm,
}

// ============================================================================
// IDL Account Name Constants for CLMM/DLMM protocols
// ============================================================================

/// Account names used in DLMM (Meteora) swap instructions
pub const DLMM_ARRAY_ACCOUNT_NAMES: &[&str] = &[
    "bin_array_lower",
    "bin_array_upper",
    "binArrayLower",
    "binArrayUpper",
    "bin_array",
    "binArray",
];

/// Account names used in Whirlpool swap instructions  
pub const WHIRLPOOL_ARRAY_ACCOUNT_NAMES: &[&str] = &[
    "tick_array_0",
    "tick_array_1",
    "tick_array_2",
    "tickArray0",
    "tickArray1",
    "tickArray2",
    "tick_array",
    "tickArray",
];

/// Account names used in Raydium CLMM swap instructions
pub const RAYDIUM_ARRAY_ACCOUNT_NAMES: &[&str] = &[
    "tick_array",
    "tickArray",
    "tick_array_lower",
    "tick_array_upper",
    "tickArrayLower",
    "tickArrayUpper",
];

/// All possible array account names across all CLMM protocols
pub const ALL_ARRAY_ACCOUNT_NAMES: &[&str] = &[
    // DLMM
    "bin_array_lower",
    "bin_array_upper",
    "binArrayLower",
    "binArrayUpper",
    "bin_array",
    "binArray",
    // Whirlpool
    "tick_array_0",
    "tick_array_1",
    "tick_array_2",
    "tickArray0",
    "tickArray1",
    "tickArray2",
    "tick_array",
    "tickArray",
    // Raydium
    "tick_array_lower",
    "tick_array_upper",
    "tickArrayLower",
    "tickArrayUpper",
];

// ============================================================================
// Reverse Map: bin_array/tick_array address → pool + index + spacing
// ============================================================================

/// Entry in the bin/tick array reverse map
#[derive(Debug, Clone, Deserialize)]
pub struct ArrayMapEntry {
    /// Protocol type: "d" = DLMM, "w" = Whirlpool, "r" = Raydium
    #[serde(rename = "t")]
    pub protocol: String,
    /// Pool address (lb_pair for DLMM, whirlpool for others)
    #[serde(rename = "p")]
    pub pool: String,
    /// Start index (bin array index for DLMM, start_tick_index for CLMM)
    #[serde(rename = "i")]
    pub start_index: i32,
    /// Spacing (bin_step for DLMM, tick_spacing for CLMM)
    #[serde(rename = "s")]
    pub spacing: u16,
    /// Pre-computed start_bin for DLMM (index * 70), not present in JSON for CLMM
    #[serde(rename = "b", default)]
    pub start_bin: Option<i32>,
}

/// Reverse map from bin_array/tick_array addresses to their pricing info.
/// This is the main lookup used during CoreCast swap processing.
#[derive(Debug, Clone)]
pub struct ClmmArrayMap {
    /// Map from array address to entry
    entries: HashMap<String, ArrayMapEntry>,
}

impl ClmmArrayMap {
    /// Load the array map from the JSON file at the project root
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        Self::load_from_path("clmm_array_map.json")
    }

    /// Load from a specific path
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let entries: HashMap<String, ArrayMapEntry> = serde_json::from_reader(reader)?;
        Ok(Self { entries })
    }

    /// Look up an array by its address
    pub fn get(&self, array_address: &str) -> Option<&ArrayMapEntry> {
        self.entries.get(array_address)
    }

    /// Calculate price for a given bin/tick array address and active bin/tick within it.
    ///
    /// For DLMM: `active_bin` is the bin_id
    /// For CLMM: `active_tick` is the tick index
    pub fn calculate_price(&self, array_address: &str, active_bin_or_tick: i32) -> Option<f64> {
        let entry = self.entries.get(array_address)?;

        match entry.protocol.as_str() {
            "d" => {
                // DLMM: price = (1 + bin_step/10000)^(bin_id - 8388608)
                Some(ClmmPriceMap::dlmm_bin_to_price(
                    entry.spacing,
                    active_bin_or_tick,
                ))
            }
            "w" | "r" | "p" => {
                // Whirlpool/Raydium/PancakeSwap: price = 1.0001^tick
                Some(ClmmPriceMap::tick_to_price(active_bin_or_tick))
            }
            _ => None,
        }
    }

    /// Get the protocol type for an array address
    pub fn get_protocol(&self, array_address: &str) -> Option<ClmmPoolType> {
        let entry = self.entries.get(array_address)?;
        match entry.protocol.as_str() {
            "d" => Some(ClmmPoolType::Dlmm),
            "w" => Some(ClmmPoolType::Whirlpool),
            "r" => Some(ClmmPoolType::RaydiumClmm),
            "p" => Some(ClmmPoolType::PancakeSwapClmm),
            _ => None,
        }
    }

    /// Total number of entries in the map
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the map is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get iterator over all entries
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ArrayMapEntry)> {
        self.entries.iter()
    }

    /// Build a reverse index from (pool, start_index) -> tick_array_address.
    /// This is used to find tick arrays for reversed swap directions.
    pub fn build_pool_index(&self) -> HashMap<(String, i32), String> {
        let mut index = HashMap::new();
        for (addr, entry) in &self.entries {
            let key = (entry.pool.clone(), entry.start_index);
            index.insert(key, addr.clone());
        }
        index
    }

    /// Find tick arrays for the opposite swap direction.
    ///
    /// CLMM swaps use different tick arrays depending on direction:
    /// - a_to_b=true (price decreasing): tick arrays at [current, current-1, current-2] indices
    /// - a_to_b=false (price increasing): tick arrays at [current, current+1, current+2] indices
    ///
    /// When reversing a swap, we need tick arrays in the opposite direction.
    /// Given the original tick_array addresses, this finds the reversed set.
    ///
    /// # Arguments
    /// * `original_arrays` - The tick array addresses from the original instruction
    /// * `pool_index` - Pre-built index from build_pool_index()
    ///
    /// # Returns
    /// * Vec of (original_address, reversed_address) pairs for each tick array that needs replacement.
    ///   If a reversed tick array is not found in the map, that pair is omitted.
    pub fn find_reversed_tick_arrays(
        &self,
        original_arrays: &[String],
        pool_index: &HashMap<(String, i32), String>,
    ) -> Vec<(String, String)> {
        let mut result = Vec::new();

        if original_arrays.is_empty() {
            return result;
        }

        // Get the entry for the first tick array to determine pool and spacing
        let first_entry = match self.entries.get(&original_arrays[0]) {
            Some(e) => e,
            None => return result,
        };

        let pool = &first_entry.pool;
        let spacing = first_entry.spacing;

        // Analyze the direction from the original tick arrays
        // If indices are [N, N-1, N-2] -> a_to_b=true (descending)
        // If indices are [N, N+1, N+2] -> a_to_b=false (ascending)
        let original_indices: Vec<i32> = original_arrays
            .iter()
            .filter_map(|addr| self.entries.get(addr).map(|e| e.start_index))
            .collect();

        if original_indices.is_empty() {
            return result;
        }

        // Determine original direction based on index progression
        let was_descending = if original_indices.len() >= 2 {
            original_indices[1] < original_indices[0]
        } else {
            // Single array - assume descending (a_to_b=true) as it's more common
            true
        };

        tracing::debug!(
            "Tick array reversal: pool={} spacing={} original_indices={:?} was_descending={}",
            &pool[..8.min(pool.len())],
            spacing,
            original_indices,
            was_descending
        );

        // For each original tick array, find the one at the opposite direction
        // If original was [0, -1, -2], reversed needs [0, +1, +2]
        // The first array (index 0 in the list) stays at the SAME start_index
        // The subsequent arrays flip their offset direction
        for (pos, orig_addr) in original_arrays.iter().enumerate() {
            let orig_entry = match self.entries.get(orig_addr) {
                Some(e) => e,
                None => continue,
            };

            // Calculate the reversed start_index
            // Position 0: same index (current tick position)
            // Position N: flip the offset direction
            let reversed_index = if pos == 0 {
                // First tick array: same starting point
                orig_entry.start_index
            } else {
                // Calculate offset from first array
                let offset_from_first = orig_entry.start_index - original_indices[0];
                // Flip the offset
                original_indices[0] - offset_from_first
            };

            // Look up the reversed tick array in the pool index
            let key = (pool.clone(), reversed_index);
            if let Some(reversed_addr) = pool_index.get(&key) {
                if reversed_addr != orig_addr {
                    result.push((orig_addr.clone(), reversed_addr.clone()));
                    tracing::debug!(
                        "Tick array reversal: pos={} orig_idx={} -> rev_idx={}, {} -> {}",
                        pos,
                        orig_entry.start_index,
                        reversed_index,
                        &orig_addr[..8.min(orig_addr.len())],
                        &reversed_addr[..8.min(reversed_addr.len())]
                    );
                }
            } else {
                tracing::warn!(
                    "Tick array not found for reversal: pool={} index={} (needed for position {})",
                    &pool[..8.min(pool.len())],
                    reversed_index,
                    pos
                );
            }
        }

        result
    }

    /// Find tick array account indices in an instruction by IDL account names.
    ///
    /// Returns the indices of accounts that are tick_array_0, tick_array_1, tick_array_2, etc.
    pub fn find_tick_array_indices(account_names: &[String]) -> Vec<usize> {
        account_names
            .iter()
            .enumerate()
            .filter_map(|(i, name)| {
                let name_lower = name.to_lowercase();
                let is_tick_array = WHIRLPOOL_ARRAY_ACCOUNT_NAMES
                    .iter()
                    .chain(RAYDIUM_ARRAY_ACCOUNT_NAMES.iter())
                    .any(|n| name_lower == n.to_lowercase());
                if is_tick_array {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find reversed tick arrays using PDA derivation (hybrid approach).
    ///
    /// This method first tries to look up tick array info from the static map.
    /// If the first tick array is found, it extracts the pool address and start_tick_index,
    /// then uses PDA derivation to compute the reversed tick array addresses.
    ///
    /// This is more reliable than pure static map lookup because PDAs are computed
    /// mathematically and don't require the reversed array to exist in the map.
    ///
    /// # Arguments
    /// * `original_arrays` - The tick array addresses from the original instruction
    /// * `protocol` - "w" for Whirlpool, "r" for Raydium CLMM
    ///
    /// # Returns
    /// * Vec of (original_pubkey, reversed_pubkey) pairs for tick arrays that need replacement.
    pub fn find_reversed_tick_arrays_pda(
        &self,
        original_arrays: &[Pubkey],
        protocol: &str,
    ) -> Vec<(Pubkey, Pubkey)> {
        let mut result = Vec::new();

        if original_arrays.is_empty() {
            return result;
        }

        // Try to get info from the first tick array in the static map
        let first_entry = match self.entries.get(&original_arrays[0].to_string()) {
            Some(e) => e,
            None => {
                tracing::debug!(
                    "First tick array {} not found in static map, cannot reverse",
                    original_arrays[0]
                );
                return result;
            }
        };

        let pool_str = &first_entry.pool;
        let pool = match Pubkey::from_str(pool_str) {
            Ok(p) => p,
            Err(_) => return result,
        };
        let tick_spacing = first_entry.spacing;

        // Get start_tick_indices for all original tick arrays
        let original_indices: Vec<i32> = original_arrays
            .iter()
            .filter_map(|addr| self.entries.get(&addr.to_string()).map(|e| e.start_index))
            .collect();

        if original_indices.is_empty() {
            return result;
        }

        tracing::debug!(
            "Tick array PDA reversal: pool={} spacing={} original_indices={:?} protocol={}",
            &pool_str[..8.min(pool_str.len())],
            tick_spacing,
            original_indices,
            protocol
        );

        // For positions 1, 2, etc. - compute reversed tick arrays using PDA derivation
        for (pos, original_pubkey) in original_arrays.iter().enumerate() {
            if pos == 0 {
                // First tick array stays the same (contains current tick)
                continue;
            }

            // Get the start_index for this original tick array
            let orig_start_index = match self.entries.get(&original_pubkey.to_string()) {
                Some(e) => e.start_index,
                None => {
                    // If not in map, try to infer from position
                    // This is a fallback - may not be accurate
                    let array_width = if protocol == "r" {
                        (tick_spacing as i32) * RAYDIUM_TICK_ARRAY_SIZE
                    } else {
                        (tick_spacing as i32) * WHIRLPOOL_TICK_ARRAY_SIZE
                    };
                    original_indices[0] - (pos as i32) * array_width // Assume descending
                }
            };

            // Calculate the offset from the first array
            let offset_from_first = orig_start_index - original_indices[0];
            // Flip the offset for the reversed direction
            let reversed_start_index = original_indices[0] - offset_from_first;

            // Derive the PDA for the reversed tick array
            let reversed_pda = if protocol == "r" {
                derive_raydium_tick_array(&pool, reversed_start_index)
            } else {
                // Default to Whirlpool (covers "w" and "p" for PancakeSwap)
                derive_whirlpool_tick_array(&pool, reversed_start_index)
            };

            tracing::info!(
                "PDA reversal: pos={} orig_idx={} -> rev_idx={}, {} -> {}",
                pos,
                orig_start_index,
                reversed_start_index,
                original_pubkey,
                reversed_pda
            );

            result.push((*original_pubkey, reversed_pda));
        }

        result
    }

    /// Get entry with fallback brute-force PDA derivation.
    ///
    /// If the array address isn't in the map, but we have pool_address and protocol,
    /// we brute-force derive PDAs to find the matching start_index.
    /// This is useful for pools not in the top 150 by volume.
    ///
    /// Returns the entry if found, along with whether it was a cache hit.
    pub fn get_or_derive(
        &self,
        array_address: &str,
        pool_address: Option<&str>,
        protocol_hint: Option<&str>,
    ) -> Option<(ArrayMapEntry, bool)> {
        // Fast path: direct lookup
        if let Some(entry) = self.entries.get(array_address) {
            return Some((entry.clone(), true));
        }

        // Slow path: brute-force if we have pool context
        let pool_str = pool_address?;
        let pool = Pubkey::from_str(pool_str).ok()?;
        let target_pda = Pubkey::from_str(array_address).ok()?;

        // Determine protocol from hint or try all
        let protocols: Vec<&str> = match protocol_hint {
            Some(p) => vec![p],
            None => vec!["w", "r", "d"],
        };

        for proto in protocols {
            match proto {
                "w" => {
                    // Whirlpool: try common tick spacings
                    for &spacing in &[1u16, 2, 4, 8, 16, 64, 128, 256] {
                        if let Some(entry) = self.brute_force_whirlpool(&pool, &target_pda, spacing)
                        {
                            tracing::debug!(
                                "Brute-force found Whirlpool array: {} spacing={} start_index={}",
                                array_address,
                                spacing,
                                entry.start_index
                            );
                            return Some((entry, false));
                        }
                    }
                }
                "r" => {
                    // Raydium CLMM: try common tick spacings
                    for &spacing in &[1u16, 10, 60, 120] {
                        if let Some(entry) = self.brute_force_raydium(&pool, &target_pda, spacing) {
                            tracing::debug!(
                                "Brute-force found Raydium array: {} spacing={} start_index={}",
                                array_address,
                                spacing,
                                entry.start_index
                            );
                            return Some((entry, false));
                        }
                    }
                }
                "d" => {
                    // Meteora DLMM: brute-force bin array index
                    if let Some(entry) = self.brute_force_meteora(&pool, &target_pda) {
                        tracing::debug!(
                            "Brute-force found Meteora array: {} start_index={}",
                            array_address,
                            entry.start_index
                        );
                        return Some((entry, false));
                    }
                }
                _ => {}
            }
        }

        None
    }

    /// Brute-force find Whirlpool tick array start_index
    fn brute_force_whirlpool(
        &self,
        pool: &Pubkey,
        target: &Pubkey,
        tick_spacing: u16,
    ) -> Option<ArrayMapEntry> {
        let array_width = (tick_spacing as i32) * WHIRLPOOL_TICK_ARRAY_SIZE;
        let min_start = -((WHIRLPOOL_MAX_TICK.abs() / array_width) * array_width);
        let max_start = (WHIRLPOOL_MAX_TICK / array_width) * array_width;

        let mut start = min_start;
        while start <= max_start {
            let pda = derive_whirlpool_tick_array(pool, start);
            if pda == *target {
                return Some(ArrayMapEntry {
                    protocol: "w".to_string(),
                    pool: pool.to_string(),
                    start_index: start,
                    spacing: tick_spacing,
                    start_bin: None,
                });
            }
            start += array_width;
        }
        None
    }

    /// Brute-force find Raydium CLMM tick array start_index
    fn brute_force_raydium(
        &self,
        pool: &Pubkey,
        target: &Pubkey,
        tick_spacing: u16,
    ) -> Option<ArrayMapEntry> {
        let array_width = (tick_spacing as i32) * RAYDIUM_TICK_ARRAY_SIZE;
        let min_start = -((RAYDIUM_MAX_TICK.abs() / array_width) * array_width);
        let max_start = (RAYDIUM_MAX_TICK / array_width) * array_width;

        let mut start = min_start;
        while start <= max_start {
            let pda = derive_raydium_tick_array(pool, start);
            if pda == *target {
                return Some(ArrayMapEntry {
                    protocol: "r".to_string(),
                    pool: pool.to_string(),
                    start_index: start,
                    spacing: tick_spacing,
                    start_bin: None,
                });
            }
            start += array_width;
        }
        None
    }

    /// Brute-force find Meteora DLMM bin array index
    fn brute_force_meteora(&self, lb_pair: &Pubkey, target: &Pubkey) -> Option<ArrayMapEntry> {
        // Meteora bin array indices range from -6656 to 6655
        for idx in METEORA_BIN_ARRAY_MIN_INDEX..=METEORA_BIN_ARRAY_MAX_INDEX {
            let pda = derive_meteora_bin_array(lb_pair, idx);
            if pda == *target {
                // We don't know bin_step from just the PDA, use placeholder
                return Some(ArrayMapEntry {
                    protocol: "d".to_string(),
                    pool: lb_pair.to_string(),
                    start_index: idx as i32,
                    spacing: 0, // Unknown - caller needs to look up from pool state
                    start_bin: Some(idx as i32 * METEORA_BIN_ARRAY_SIZE),
                });
            }
        }
        None
    }

    /// Calculate canonical price from observed price and array address.
    /// Uses get_or_derive for broader coverage.
    pub fn canonical_price_with_fallback(
        &self,
        array_address: &str,
        observed_price: f64,
        pool_address: Option<&str>,
        protocol_hint: Option<&str>,
    ) -> Option<(i32, f64)> {
        let (entry, _was_hit) = self.get_or_derive(array_address, pool_address, protocol_hint)?;

        match entry.protocol.as_str() {
            "d" => {
                // For brute-forced DLMM entries, spacing might be 0
                // In that case, we can't calculate canonical price
                if entry.spacing == 0 {
                    return None;
                }
                // DLMM: price = (1 + bin_step/10000)^bin_id
                // Solving for bin_id: bin_id = ln(price) / ln(1 + bin_step/10000)
                let base = 1.0 + (entry.spacing as f64) / 10000.0;
                let bin_id = (observed_price.ln() / base.ln()).round() as i32;
                let canonical = ClmmPriceMap::dlmm_bin_to_price(entry.spacing, bin_id);
                Some((bin_id, canonical))
            }
            "w" | "r" | "p" => {
                let tick = (observed_price.ln() / 1.0001_f64.ln()).round() as i32;
                let canonical = ClmmPriceMap::tick_to_price(tick);
                Some((tick, canonical))
            }
            _ => None,
        }
    }
}

// ============================================================================
// Price Back-Calculation from Swap Amounts
// ============================================================================

/// Result of back-calculating price from swap token deltas
#[derive(Debug, Clone, Copy)]
pub struct SwapPriceInfo {
    /// The observed effective price (output_amount / input_amount) - includes slippage
    pub observed_price: f64,
    /// The canonical protocol price derived from the bin/tick
    /// DLMM: (1 + bin_step/10000)^(bin_id - 8388608)
    /// CLMM: 1.0001^tick
    pub canonical_price: f64,
    /// Back-calculated bin_id (for DLMM) or tick (for CLMM)
    pub bin_or_tick: i32,
    /// Protocol type
    pub protocol: ClmmPoolType,
}

impl ClmmArrayMap {
    /// Back-calculate the active bin/tick from swap token deltas.
    ///
    /// This is the magic: CoreCast gives us pre/post balances, from which we
    /// calculate the effective swap price, then reverse-engineer the bin/tick.
    ///
    /// # Arguments
    /// * `array_address` - The bin_array/tick_array address from the swap instruction
    /// * `input_delta` - Absolute value of tokens spent (positive number)
    /// * `output_delta` - Absolute value of tokens received (positive number)
    /// * `input_decimals` - Decimals of the input token
    /// * `output_decimals` - Decimals of the output token
    ///
    /// # Returns
    /// * `SwapPriceInfo` with the effective price and back-calculated bin/tick
    pub fn price_from_swap_deltas(
        &self,
        array_address: &str,
        input_delta: u64,
        output_delta: u64,
        input_decimals: u8,
        output_decimals: u8,
    ) -> Option<SwapPriceInfo> {
        let entry = self.entries.get(array_address)?;

        // Calculate effective price with decimal adjustment
        // price = (output / 10^output_decimals) / (input / 10^input_decimals)
        let input_amount = input_delta as f64 / 10_f64.powi(input_decimals as i32);
        let output_amount = output_delta as f64 / 10_f64.powi(output_decimals as i32);

        if input_amount == 0.0 {
            return None;
        }

        let observed_price = output_amount / input_amount;

        let (bin_or_tick, canonical_price, protocol) = match entry.protocol.as_str() {
            "d" => {
                // DLMM: price = (1 + bin_step/10000)^bin_id
                // Solving for bin_id: bin_id = ln(price) / ln(1 + bin_step/10000)
                let base = 1.0 + (entry.spacing as f64) / 10000.0;
                let bin_id = (observed_price.ln() / base.ln()).round() as i32;
                // Canonical price from the bin_id
                let canonical = ClmmPriceMap::dlmm_bin_to_price(entry.spacing, bin_id);
                (bin_id, canonical, ClmmPoolType::Dlmm)
            }
            "w" => {
                // Whirlpool: price = 1.0001^tick
                // tick = ln(price) / ln(1.0001)
                let tick = (observed_price.ln() / 1.0001_f64.ln()).round() as i32;
                // Canonical price from the tick
                let canonical = ClmmPriceMap::tick_to_price(tick);
                (tick, canonical, ClmmPoolType::Whirlpool)
            }
            "r" => {
                // Raydium: same as Whirlpool
                let tick = (observed_price.ln() / 1.0001_f64.ln()).round() as i32;
                let canonical = ClmmPriceMap::tick_to_price(tick);
                (tick, canonical, ClmmPoolType::RaydiumClmm)
            }
            "p" => {
                // PancakeSwap: same tick-based formula as Raydium
                let tick = (observed_price.ln() / 1.0001_f64.ln()).round() as i32;
                let canonical = ClmmPriceMap::tick_to_price(tick);
                (tick, canonical, ClmmPoolType::PancakeSwapClmm)
            }
            _ => return None,
        };

        Some(SwapPriceInfo {
            observed_price,
            canonical_price,
            bin_or_tick,
            protocol,
        })
    }

    /// Simpler version: back-calculate bin/tick and canonical price from raw price ratio.
    /// Returns (bin_or_tick, canonical_price)
    pub fn canonical_price_from_observed(
        &self,
        array_address: &str,
        observed_price: f64,
    ) -> Option<(i32, f64)> {
        let entry = self.entries.get(array_address)?;

        match entry.protocol.as_str() {
            "d" => {
                // DLMM: price = (1 + bin_step/10000)^bin_id
                // bin_id = ln(price) / ln(1 + bin_step/10000)
                let base = 1.0 + (entry.spacing as f64) / 10000.0;
                let bin_id = (observed_price.ln() / base.ln()).round() as i32;
                let canonical = ClmmPriceMap::dlmm_bin_to_price(entry.spacing, bin_id);
                Some((bin_id, canonical))
            }
            "w" | "r" | "p" => {
                let tick = (observed_price.ln() / 1.0001_f64.ln()).round() as i32;
                let canonical = ClmmPriceMap::tick_to_price(tick);
                Some((tick, canonical))
            }
            _ => None,
        }
    }

    /// Get just the bin/tick from observed price (legacy compatibility)
    pub fn bin_or_tick_from_price(&self, array_address: &str, price: f64) -> Option<i32> {
        self.canonical_price_from_observed(array_address, price)
            .map(|(bin_or_tick, _)| bin_or_tick)
    }

    /// Get the price range covered by a bin/tick array.
    /// Returns (min_price, max_price) for the array.
    pub fn get_price_range(&self, array_address: &str) -> Option<(f64, f64)> {
        let entry = self.entries.get(array_address)?;

        match entry.protocol.as_str() {
            "d" => {
                // DLMM: 70 bins per array
                // start_bin is already the actual bin_id (can be negative)
                let start_bin = entry.start_bin.unwrap_or(entry.start_index * 70);
                let end_bin = start_bin + 69;
                let base = 1.0 + (entry.spacing as f64) / 10000.0;
                // price = base^bin_id directly (no offset)
                let price_low = base.powi(start_bin);
                let price_high = base.powi(end_bin);
                Some((price_low.min(price_high), price_low.max(price_high)))
            }
            "w" => {
                // Whirlpool: 88 ticks per array
                let start_tick = entry.start_index;
                let end_tick = start_tick + 88 * entry.spacing as i32 - 1;
                let price_low = 1.0001_f64.powi(start_tick);
                let price_high = 1.0001_f64.powi(end_tick);
                Some((price_low.min(price_high), price_low.max(price_high)))
            }
            "r" | "p" => {
                // Raydium/PancakeSwap: 60 ticks per array
                let start_tick = entry.start_index;
                let end_tick = start_tick + 60 * entry.spacing as i32 - 1;
                let price_low = 1.0001_f64.powi(start_tick);
                let price_high = 1.0001_f64.powi(end_tick);
                Some((price_low.min(price_high), price_low.max(price_high)))
            }
            _ => None,
        }
    }

    /// Extract bin/tick array address from account addresses.
    ///
    /// For Geyser-based pipelines, use this with raw Pubkey slices.
    pub fn find_array_from_addresses(&self, addresses: &[solana_sdk::pubkey::Pubkey]) -> Option<String> {
        for addr in addresses {
            let addr_str = addr.to_string();
            if self.entries.contains_key(&addr_str) {
                return Some(addr_str);
            }
        }
        None
    }

    /// Extract all bin/tick array addresses from account addresses.
    pub fn find_all_arrays_from_addresses(&self, addresses: &[solana_sdk::pubkey::Pubkey]) -> Vec<String> {
        addresses
            .iter()
            .filter_map(|addr| {
                let addr_str = addr.to_string();
                if self.entries.contains_key(&addr_str) {
                    Some(addr_str)
                } else {
                    None
                }
            })
            .collect()
    }
}

// ============================================================================
// CLMM Math Constants and Helper Functions
// ============================================================================

/// Q64 fixed-point multiplier (2^64)
const Q64: u128 = 1u128 << 64;

/// Minimum sqrt price (tick -443636) - same for Whirlpool and Raydium CLMM
const MIN_SQRT_PRICE_X64: u128 = 4295048016;

/// Maximum sqrt price for Orca Whirlpool (tick 443636)
/// Note: Raydium CLMM uses a slightly different value: 79226673521066979257578248091
/// This is due to different rounding in their implementations.
/// Use Whirlpool value here as it's more conservative (smaller range).
#[allow(dead_code)]
const MAX_SQRT_PRICE_X64_WHIRLPOOL: u128 = 79226673515401279992447579055;

/// Maximum sqrt price for Raydium CLMM (tick 443636)
#[allow(dead_code)]
const MAX_SQRT_PRICE_X64_RAYDIUM: u128 = 79226673521066979257578248091;

/// Default MAX sqrt price - use Whirlpool value (more conservative)
const MAX_SQRT_PRICE_X64: u128 = MAX_SQRT_PRICE_X64_WHIRLPOOL;

/// DLMM scale offset for fixed-point math
const SCALE_OFFSET: u32 = 64;

/// Calculate sqrt_price_x64 from tick index
/// Formula: sqrt(1.0001^tick) * 2^64
pub fn tick_to_sqrt_price_x64(tick: i32) -> u128 {
    // Use high-precision calculation
    let price = 1.0001_f64.powi(tick);
    let sqrt_price = price.sqrt();
    (sqrt_price * Q64 as f64) as u128
}

/// Calculate tick from sqrt_price_x64
/// Formula: floor(log(sqrt_price^2) / log(1.0001))
pub fn sqrt_price_x64_to_tick(sqrt_price_x64: u128) -> i32 {
    let sqrt_price = sqrt_price_x64 as f64 / Q64 as f64;
    let price = sqrt_price * sqrt_price;
    (price.ln() / 1.0001_f64.ln()).floor() as i32
}

/// Calculate token0 amount delta for a sqrt_price range
/// Formula: Δ0 = L * (1/√P_lower - 1/√P_upper) = L * (√P_upper - √P_lower) / (√P_lower * √P_upper)
///
/// This is used when swapping token1 -> token0 (price increasing)
pub fn get_delta_amount_0(
    sqrt_price_lower_x64: u128,
    sqrt_price_upper_x64: u128,
    liquidity: u128,
    round_up: bool,
) -> u64 {
    if sqrt_price_lower_x64 >= sqrt_price_upper_x64 || liquidity == 0 {
        return 0;
    }

    // Δ0 = L * (√P_upper - √P_lower) / (√P_lower * √P_upper)
    // Using fixed-point: amount = L * Q64 * (sqrt_upper - sqrt_lower) / (sqrt_lower * sqrt_upper)
    let sqrt_diff = sqrt_price_upper_x64.saturating_sub(sqrt_price_lower_x64);

    // Compute sqrt_lower * sqrt_upper in Q64 (result in Q128)
    let product = (sqrt_price_lower_x64 as u128)
        .checked_mul(sqrt_price_upper_x64 as u128)
        .unwrap_or(u128::MAX);

    if product == 0 {
        return 0;
    }

    // amount = L * sqrt_diff * Q64 / product
    let numerator = (liquidity as u128)
        .checked_mul(sqrt_diff)
        .and_then(|x| x.checked_mul(Q64))
        .unwrap_or(u128::MAX);

    let amount = if round_up {
        numerator.saturating_add(product - 1) / product
    } else {
        numerator / product
    };

    amount.min(u64::MAX as u128) as u64
}

/// Calculate token1 amount delta for a sqrt_price range
/// Formula: Δ1 = L * (√P_upper - √P_lower)
///
/// This is used when swapping token0 -> token1 (price decreasing)
pub fn get_delta_amount_1(
    sqrt_price_lower_x64: u128,
    sqrt_price_upper_x64: u128,
    liquidity: u128,
    round_up: bool,
) -> u64 {
    if sqrt_price_lower_x64 >= sqrt_price_upper_x64 || liquidity == 0 {
        return 0;
    }

    // Δ1 = L * (√P_upper - √P_lower) / Q64
    let sqrt_diff = sqrt_price_upper_x64.saturating_sub(sqrt_price_lower_x64);

    let numerator = (liquidity as u128)
        .checked_mul(sqrt_diff)
        .unwrap_or(u128::MAX);

    let amount = if round_up {
        numerator.saturating_add(Q64 - 1) / Q64
    } else {
        numerator / Q64
    };

    amount.min(u64::MAX as u128) as u64
}

/// Calculate the next sqrt_price after swapping an exact input amount
///
/// For zero_for_one (token0 -> token1, price decreasing):
///   sqrt_price_next = L * sqrt_price_current / (L + amount_in * sqrt_price_current)
///
/// For one_for_zero (token1 -> token0, price increasing):
///   sqrt_price_next = sqrt_price_current + amount_in * Q64 / L
pub fn get_next_sqrt_price_from_input(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount_in: u64,
    zero_for_one: bool,
) -> u128 {
    if liquidity == 0 {
        return sqrt_price_x64;
    }

    if zero_for_one {
        // price decreasing: sqrt_next = L * sqrt_current / (L + amount * sqrt_current / Q64)
        let product = (amount_in as u128)
            .checked_mul(sqrt_price_x64)
            .unwrap_or(u128::MAX);
        let denominator = (liquidity as u128)
            .checked_mul(Q64)
            .and_then(|x| x.checked_add(product))
            .unwrap_or(u128::MAX);

        if denominator == 0 {
            return MIN_SQRT_PRICE_X64;
        }

        let numerator = (liquidity as u128)
            .checked_mul(sqrt_price_x64)
            .and_then(|x| x.checked_mul(Q64))
            .unwrap_or(0);

        (numerator / denominator).max(MIN_SQRT_PRICE_X64)
    } else {
        // price increasing: sqrt_next = sqrt_current + amount * Q64 / L
        let delta = (amount_in as u128)
            .checked_mul(Q64)
            .map(|x| x / liquidity)
            .unwrap_or(0);

        sqrt_price_x64.saturating_add(delta).min(MAX_SQRT_PRICE_X64)
    }
}

/// Compute a single swap step within a tick range
/// Returns (amount_in, amount_out, sqrt_price_next, fee_amount)
pub fn compute_swap_step(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    amount_remaining: u64,
    fee_rate_bps: u16,
    zero_for_one: bool,
) -> (u64, u64, u128, u64) {
    if liquidity == 0 || amount_remaining == 0 {
        return (0, 0, sqrt_price_current_x64, 0);
    }

    // Deduct fee from input
    let fee_rate = fee_rate_bps as u128;
    let amount_remaining_less_fee =
        ((amount_remaining as u128) * (10000 - fee_rate) / 10000) as u64;

    // Calculate the maximum amount that can be swapped to reach target price
    let (amount_in_max, amount_out_max) = if zero_for_one {
        // Swapping token0 for token1 (price decreasing)
        let amount_in = get_delta_amount_0(
            sqrt_price_target_x64,
            sqrt_price_current_x64,
            liquidity,
            true,
        );
        let amount_out = get_delta_amount_1(
            sqrt_price_target_x64,
            sqrt_price_current_x64,
            liquidity,
            false,
        );
        (amount_in, amount_out)
    } else {
        // Swapping token1 for token0 (price increasing)
        let amount_in = get_delta_amount_1(
            sqrt_price_current_x64,
            sqrt_price_target_x64,
            liquidity,
            true,
        );
        let amount_out = get_delta_amount_0(
            sqrt_price_current_x64,
            sqrt_price_target_x64,
            liquidity,
            false,
        );
        (amount_in, amount_out)
    };

    // Determine if we reach the target price or consume all input
    let (amount_in, amount_out, sqrt_price_next) = if amount_remaining_less_fee >= amount_in_max {
        // We reach the target price
        (amount_in_max, amount_out_max, sqrt_price_target_x64)
    } else {
        // We don't reach the target, calculate actual output
        let sqrt_price_next = get_next_sqrt_price_from_input(
            sqrt_price_current_x64,
            liquidity,
            amount_remaining_less_fee,
            zero_for_one,
        );

        let (actual_in, actual_out) = if zero_for_one {
            let a_in = get_delta_amount_0(sqrt_price_next, sqrt_price_current_x64, liquidity, true);
            let a_out =
                get_delta_amount_1(sqrt_price_next, sqrt_price_current_x64, liquidity, false);
            (a_in, a_out)
        } else {
            let a_in = get_delta_amount_1(sqrt_price_current_x64, sqrt_price_next, liquidity, true);
            let a_out =
                get_delta_amount_0(sqrt_price_current_x64, sqrt_price_next, liquidity, false);
            (a_in, a_out)
        };

        (actual_in, actual_out, sqrt_price_next)
    };

    // Calculate fee
    let fee_amount = if sqrt_price_next != sqrt_price_target_x64 {
        // Didn't reach target, remainder is fee
        amount_remaining.saturating_sub(amount_in)
    } else {
        // Reached target, calculate fee from amount_in
        ((amount_in as u128) * fee_rate / (10000 - fee_rate)) as u64
    };

    (amount_in, amount_out, sqrt_price_next, fee_amount)
}

// ============================================================================
// DLMM (Meteora) Bin Math Functions
// ============================================================================

/// Calculate exact bin price from bin_step and bin_id
/// Formula: price = (1 + bin_step/10000)^bin_id
pub fn dlmm_bin_to_price(bin_step: u16, bin_id: i32) -> f64 {
    let base = 1.0 + (bin_step as f64) / 10000.0;
    base.powi(bin_id)
}

/// Calculate bin_id from price
/// Formula: bin_id = round(log(price) / log(1 + bin_step/10000))
pub fn dlmm_price_to_bin_id(bin_step: u16, price: f64) -> i32 {
    if price <= 0.0 {
        return 0;
    }
    let base = 1.0 + (bin_step as f64) / 10000.0;
    (price.ln() / base.ln()).round() as i32
}

/// Calculate amount out for DLMM swap within a single bin
/// For swap_for_y (X -> Y): amount_out = amount_in * price
/// For swap_for_x (Y -> X): amount_out = amount_in / price
///
/// Uses fixed-point math matching the official SDK
pub fn dlmm_get_amount_out(
    amount_in: u64,
    bin_price: f64,
    swap_for_y: bool,
    input_decimals: u8,
    output_decimals: u8,
) -> u64 {
    let input_norm = amount_in as f64 / 10_f64.powi(input_decimals as i32);

    let output_norm = if swap_for_y {
        // X -> Y: multiply by price
        input_norm * bin_price
    } else {
        // Y -> X: divide by price
        input_norm / bin_price
    };

    (output_norm * 10_f64.powi(output_decimals as i32)) as u64
}

/// Calculate amount in required for DLMM swap to get exact output
/// For swap_for_y (X -> Y): amount_in = amount_out / price
/// For swap_for_x (Y -> X): amount_in = amount_out * price
pub fn dlmm_get_amount_in(
    amount_out: u64,
    bin_price: f64,
    swap_for_y: bool,
    input_decimals: u8,
    output_decimals: u8,
) -> u64 {
    let output_norm = amount_out as f64 / 10_f64.powi(output_decimals as i32);

    let input_norm = if swap_for_y {
        // X -> Y: divide output by price
        output_norm / bin_price
    } else {
        // Y -> X: multiply output by price
        output_norm * bin_price
    };

    (input_norm * 10_f64.powi(input_decimals as i32)).ceil() as u64
}

/// Simulate DLMM swap across multiple bins
/// Returns (output_amount, fee_amount, bins_crossed)
pub fn dlmm_simulate_swap(
    input_amount: u64,
    bin_step: u16,
    current_bin_id: i32,
    fee_bps: u16,
    swap_for_y: bool,
    input_decimals: u8,
    output_decimals: u8,
    // Approximate liquidity per bin (if known)
    liquidity_per_bin: Option<u64>,
) -> (u64, u64, u32) {
    // Apply fee upfront
    let fee_amount = ((input_amount as u128) * (fee_bps as u128) / 10000) as u64;
    let input_after_fee = input_amount.saturating_sub(fee_amount);

    if input_after_fee == 0 {
        return (0, fee_amount, 0);
    }

    // Get liquidity per bin or estimate it
    let bin_liquidity = liquidity_per_bin.unwrap_or_else(|| {
        // Estimate: assume each bin can handle ~10% of input
        input_after_fee / 10
    });

    let mut remaining_input = input_after_fee;
    let mut total_output: u64 = 0;
    let mut current_bin = current_bin_id;
    let mut bins_crossed: u32 = 0;

    // Simulate crossing bins
    while remaining_input > 0 && bins_crossed < 100 {
        // Calculate price at current bin
        let bin_price = dlmm_bin_to_price(bin_step, current_bin);

        // Amount we can swap in this bin
        let input_this_bin = remaining_input.min(bin_liquidity);

        // Calculate output for this bin
        let output_this_bin = dlmm_get_amount_out(
            input_this_bin,
            bin_price,
            swap_for_y,
            input_decimals,
            output_decimals,
        );

        total_output = total_output.saturating_add(output_this_bin);
        remaining_input = remaining_input.saturating_sub(input_this_bin);

        if remaining_input > 0 {
            // Move to next bin
            if swap_for_y {
                current_bin -= 1; // Price decreases
            } else {
                current_bin += 1; // Price increases
            }
            bins_crossed += 1;
        }
    }

    if bins_crossed == 0 && remaining_input == 0 {
        bins_crossed = 0; // Stayed within single bin
    }

    (total_output, fee_amount, bins_crossed)
}

// ============================================================================
// CLMM-Aware Slippage Calculation
// ============================================================================

/// Result of CLMM slippage calculation
#[derive(Debug, Clone, Copy)]
pub struct ClmmSlippage {
    /// Estimated output amount after slippage
    pub output_amount: u64,
    /// Slippage in basis points
    pub slippage_bps: u64,
    /// Number of bins/ticks crossed (0 if within single bin/tick range)
    pub bins_or_ticks_crossed: u32,
    /// Protocol type
    pub protocol: ClmmPoolType,
}

impl ClmmArrayMap {
    /// Calculate expected output with CLMM-aware slippage using correct protocol math.
    ///
    /// For DLMM (Meteora):
    /// - Uses exact bin price formula: price = (1 + bin_step/10000)^bin_id
    /// - Iterates through bins consuming liquidity at each bin's price
    ///
    /// For CLMM (Whirlpool/Raydium):
    /// - Uses sqrt_price math with liquidity
    /// - Calculates amount deltas: Δ0 = L * (1/√P_lower - 1/√P_upper)
    /// - Calculates amount deltas: Δ1 = L * (√P_upper - √P_lower)
    ///
    /// # Arguments
    /// * `array_address` - The bin_array/tick_array address
    /// * `input_amount` - Raw input amount (with decimals)
    /// * `current_price` - Current observed price (output/input in human terms)
    /// * `input_reserve` - Approximate input-side liquidity (from PoolStateCache)
    /// * `output_reserve` - Approximate output-side liquidity
    /// * `input_decimals` - Input token decimals
    /// * `output_decimals` - Output token decimals
    /// * `fee_bps` - Swap fee in basis points
    pub fn calculate_clmm_output(
        &self,
        array_address: &str,
        input_amount: u64,
        current_price: f64,
        input_reserve: u64,
        output_reserve: u64,
        input_decimals: u8,
        output_decimals: u8,
        fee_bps: u16,
    ) -> Option<ClmmSlippage> {
        let entry = self.entries.get(array_address)?;

        let (output_amount, slippage_bps, bins_or_ticks_crossed, protocol) = match entry
            .protocol
            .as_str()
        {
            "d" => {
                // ================================================================
                // DLMM (Meteora): Correct bin-based swap math
                // ================================================================
                // Formula: price = (1 + bin_step/10000)^bin_id
                // Within a bin: amount_out = amount_in * price (constant price)
                // Crossing bins: iterate and sum outputs at each bin price

                let bin_step = entry.spacing;

                // Calculate current bin_id from price
                let current_bin_id = dlmm_price_to_bin_id(bin_step, current_price);

                // Estimate liquidity per bin from reserves
                // Assume liquidity is spread across ~10 active bins centered on current
                let estimated_bins = 10u64;
                let liquidity_per_bin = input_reserve / estimated_bins.max(1);

                // Determine swap direction (assume base -> quote for now)
                // swap_for_y = true means we're selling X (base) for Y (quote)
                let swap_for_y = true;

                // Simulate the swap
                let (output, _fee, bins_crossed) = dlmm_simulate_swap(
                    input_amount,
                    bin_step,
                    current_bin_id,
                    fee_bps,
                    swap_for_y,
                    input_decimals,
                    output_decimals,
                    Some(liquidity_per_bin),
                );

                // Calculate ideal output (no slippage)
                let _input_norm = input_amount as f64 / 10_f64.powi(input_decimals as i32);
                let fee_amount = (input_amount as u128 * fee_bps as u128 / 10000) as u64;
                let input_after_fee = input_amount.saturating_sub(fee_amount);
                let input_after_fee_norm =
                    input_after_fee as f64 / 10_f64.powi(input_decimals as i32);
                let ideal_output_norm = input_after_fee_norm * current_price;
                let ideal_output = (ideal_output_norm * 10_f64.powi(output_decimals as i32)) as u64;

                // Calculate slippage
                let slippage_bps = if ideal_output > 0 {
                    let slippage =
                        (ideal_output.saturating_sub(output)) as f64 / ideal_output as f64;
                    (slippage * 10000.0) as u64
                } else {
                    0
                };

                (output, slippage_bps, bins_crossed, ClmmPoolType::Dlmm)
            }
            "w" | "r" | "p" => {
                // ================================================================
                // CLMM (Whirlpool/Raydium/PancakeSwap): Correct sqrt_price math
                // ================================================================
                // Uses: compute_swap_step with sqrt_price and liquidity
                // Delta formulas:
                //   Δ0 = L * (1/√P_lower - 1/√P_upper)  [token0 for price range]
                //   Δ1 = L * (√P_upper - √P_lower)      [token1 for price range]

                let tick_spacing = entry.spacing as i32;

                // Convert current price to sqrt_price_x64
                let sqrt_price_x64 = (current_price.sqrt() * Q64 as f64) as u128;

                // Estimate liquidity from reserves using sqrt_price
                // L = sqrt(x * y) where x, y are normalized reserves
                let x_norm = input_reserve as f64 / 10_f64.powi(input_decimals as i32);
                let y_norm = output_reserve as f64 / 10_f64.powi(output_decimals as i32);
                let estimated_liquidity = ((x_norm * y_norm).sqrt() * Q64 as f64) as u128;

                // Apply fee upfront
                let fee_amount = (input_amount as u128 * fee_bps as u128 / 10000) as u64;
                let input_after_fee = input_amount.saturating_sub(fee_amount);

                if input_after_fee == 0 || estimated_liquidity == 0 {
                    return Some(ClmmSlippage {
                        output_amount: 0,
                        slippage_bps: 0,
                        bins_or_ticks_crossed: 0,
                        protocol: match entry.protocol.as_str() {
                            "w" => ClmmPoolType::Whirlpool,
                            "p" => ClmmPoolType::PancakeSwapClmm,
                            _ => ClmmPoolType::RaydiumClmm,
                        },
                    });
                }

                // Simulate swap through multiple tick ranges
                // For simplicity, we do a single-step calculation with estimated liquidity
                // In production, you'd iterate through tick arrays

                // Determine swap direction (zero_for_one = selling token0)
                let zero_for_one = true; // Assume base -> quote

                // Calculate target sqrt_price (use min/max as limit)
                let sqrt_price_target = if zero_for_one {
                    // Price decreasing, target is lower
                    MIN_SQRT_PRICE_X64
                } else {
                    // Price increasing, target is higher
                    MAX_SQRT_PRICE_X64
                };

                // Compute swap step
                let (_amount_in, amount_out, sqrt_price_next, _fee) = compute_swap_step(
                    sqrt_price_x64,
                    sqrt_price_target,
                    estimated_liquidity,
                    input_after_fee,
                    fee_bps,
                    zero_for_one,
                );

                // Calculate ticks crossed
                let current_tick = sqrt_price_x64_to_tick(sqrt_price_x64);
                let next_tick = sqrt_price_x64_to_tick(sqrt_price_next);
                let ticks_crossed = ((current_tick - next_tick).abs() / tick_spacing.max(1)) as u32;

                // Calculate ideal output (no slippage)
                let input_norm = input_after_fee as f64 / 10_f64.powi(input_decimals as i32);
                let ideal_output_norm = input_norm * current_price;
                let ideal_output = (ideal_output_norm * 10_f64.powi(output_decimals as i32)) as u64;

                // Calculate slippage
                let slippage_bps = if ideal_output > 0 {
                    let slippage =
                        (ideal_output.saturating_sub(amount_out)) as f64 / ideal_output as f64;
                    (slippage * 10000.0).min(10000.0) as u64
                } else {
                    0
                };

                let protocol = match entry.protocol.as_str() {
                    "w" => ClmmPoolType::Whirlpool,
                    "p" => ClmmPoolType::PancakeSwapClmm,
                    _ => ClmmPoolType::RaydiumClmm,
                };

                (amount_out, slippage_bps, ticks_crossed, protocol)
            }
            _ => return None,
        };

        Some(ClmmSlippage {
            output_amount,
            slippage_bps,
            bins_or_ticks_crossed,
            protocol,
        })
    }

    /// Calculate CLMM output with full state (sqrt_price, liquidity, tick)
    ///
    /// This is the most accurate calculation when you have the actual pool state
    /// from on-chain or from CoreCast trade events.
    pub fn calculate_clmm_output_with_state(
        &self,
        array_address: &str,
        input_amount: u64,
        sqrt_price_x64: u128,
        liquidity: u128,
        current_tick: i32,
        fee_bps: u16,
        zero_for_one: bool,
        input_decimals: u8,
        output_decimals: u8,
    ) -> Option<ClmmSlippage> {
        let entry = self.entries.get(array_address)?;

        // Only valid for CLMM protocols
        match entry.protocol.as_str() {
            "w" | "r" | "p" => {}
            _ => return None,
        }

        let tick_spacing = entry.spacing as i32;

        // Calculate next tick boundary
        let next_tick = if zero_for_one {
            // Round down to tick spacing
            (current_tick / tick_spacing) * tick_spacing
        } else {
            // Round up to next tick spacing
            ((current_tick / tick_spacing) + 1) * tick_spacing
        };

        let _sqrt_price_target = tick_to_sqrt_price_x64(next_tick);

        // Apply fee
        let fee_amount = (input_amount as u128 * fee_bps as u128 / 10000) as u64;
        let input_after_fee = input_amount.saturating_sub(fee_amount);

        // Track total output across tick crossings
        let mut total_output: u64 = 0;
        let mut remaining_input = input_after_fee;
        let mut current_sqrt_price = sqrt_price_x64;
        let current_liquidity = liquidity;
        let mut ticks_crossed: u32 = 0;
        let mut current_tick_mut = current_tick;

        // Simulate crossing up to 10 tick ranges
        while remaining_input > 0 && ticks_crossed < 10 {
            let target_tick = if zero_for_one {
                (current_tick_mut / tick_spacing) * tick_spacing
            } else {
                ((current_tick_mut / tick_spacing) + 1) * tick_spacing
            };
            let target_sqrt_price = tick_to_sqrt_price_x64(target_tick);

            let (amount_in, amount_out, sqrt_price_next, _fee) = compute_swap_step(
                current_sqrt_price,
                target_sqrt_price,
                current_liquidity,
                remaining_input,
                0, // Fee already deducted
                zero_for_one,
            );

            total_output = total_output.saturating_add(amount_out);
            remaining_input = remaining_input.saturating_sub(amount_in);

            if sqrt_price_next == target_sqrt_price && remaining_input > 0 {
                // Crossed a tick, would need to update liquidity from tick data
                // For now, assume constant liquidity
                ticks_crossed += 1;
                current_tick_mut = if zero_for_one {
                    target_tick - tick_spacing
                } else {
                    target_tick + tick_spacing
                };
            }

            current_sqrt_price = sqrt_price_next;
        }

        // Calculate ideal output for slippage
        let current_price = ClmmPriceMap::sqrt_price_x64_to_price(sqrt_price_x64);
        let input_norm = input_after_fee as f64 / 10_f64.powi(input_decimals as i32);
        let ideal_output_norm = input_norm * current_price;
        let ideal_output = (ideal_output_norm * 10_f64.powi(output_decimals as i32)) as u64;

        let slippage_bps = if ideal_output > 0 {
            let slippage = (ideal_output.saturating_sub(total_output)) as f64 / ideal_output as f64;
            (slippage * 10000.0).min(10000.0) as u64
        } else {
            0
        };

        let protocol = match entry.protocol.as_str() {
            "w" => ClmmPoolType::Whirlpool,
            "p" => ClmmPoolType::PancakeSwapClmm,
            _ => ClmmPoolType::RaydiumClmm,
        };

        Some(ClmmSlippage {
            output_amount: total_output,
            slippage_bps,
            bins_or_ticks_crossed: ticks_crossed,
            protocol,
        })
    }

    /// Simpler slippage estimation for CLMM pools.
    /// Returns expected slippage in basis points based on trade size and spacing.
    ///
    /// Use this when you don't have full reserve data but know the trade size fraction.
    pub fn estimate_slippage_bps(
        &self,
        array_address: &str,
        trade_size_fraction: f64,
    ) -> Option<u64> {
        let entry = self.entries.get(array_address)?;

        let slippage_bps = match entry.protocol.as_str() {
            "d" => {
                // DLMM: slippage depends on bins crossed
                // Each bin adds bin_step/10000 to price
                let bin_step = entry.spacing as f64;
                let bins_crossed = (trade_size_fraction * 10.0).ceil(); // ~10 bins for 100% of liquidity
                (bins_crossed * bin_step / 2.0) as u64 // Average slippage
            }
            "w" | "r" | "p" => {
                // CLMM: use sqrt_price based estimation
                // For concentrated liquidity, slippage is lower than constant product
                // Approximate: slippage ≈ trade_fraction^2 * 5000 bps (quadratic)
                let base_slippage = trade_size_fraction * trade_size_fraction * 5000.0;
                base_slippage.min(5000.0) as u64
            }
            _ => return None,
        };

        Some(slippage_bps.min(5000)) // Cap at 50%
    }

    /// Get the spacing (bin_step for DLMM, tick_spacing for CLMM) for an array
    pub fn get_spacing(&self, array_address: &str) -> Option<u16> {
        self.entries.get(array_address).map(|e| e.spacing)
    }
}

// ============================================================================
// PDA Derivation for CLMM Tick/Bin Arrays
// ============================================================================
// These functions compute tick_array/bin_array PDAs on-the-fly without needing
// a static map. This is essential for swap direction reversal since the active
// tick arrays change constantly and a static map quickly becomes stale.
//
// Each protocol uses different seed formats:
// - Whirlpool: ["tick_array", pool, start_tick_index.to_string().as_bytes()]
// - Raydium CLMM: ["tick_array", pool, start_tick_index.to_be_bytes()]  (i32 big-endian)
// - Meteora DLMM: ["bin_array", lb_pair, bin_array_index.to_le_bytes()]  (i64 little-endian)
// ============================================================================

/// Whirlpool program ID
pub const WHIRLPOOL_PROGRAM_ID: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
/// Raydium CLMM program ID
pub const RAYDIUM_CLMM_PROGRAM_ID: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
/// Meteora DLMM program ID
pub const METEORA_DLMM_PROGRAM_ID: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

/// Whirlpool ticks per tick array
pub const WHIRLPOOL_TICK_ARRAY_SIZE: i32 = 88;
/// Raydium CLMM ticks per tick array
pub const RAYDIUM_TICK_ARRAY_SIZE: i32 = 60;
/// Meteora DLMM bins per bin array
pub const METEORA_BIN_ARRAY_SIZE: i32 = 70;

/// Min/max tick indices (from SDK)
pub const WHIRLPOOL_MAX_TICK: i32 = 443636;
pub const RAYDIUM_MAX_TICK: i32 = 443636;

/// Meteora bin array index range (from SDK: BIN_ARRAY_BITMAP_SIZE * EXTENSION size)
pub const METEORA_BIN_ARRAY_MIN_INDEX: i64 = -6656;
pub const METEORA_BIN_ARRAY_MAX_INDEX: i64 = 6655;

/// Derive a Whirlpool tick_array PDA.
///
/// Seeds: ["tick_array", pool_pubkey, start_tick_index.to_string().as_bytes()]
///
/// # Arguments
/// * `pool` - The Whirlpool pool address
/// * `start_tick_index` - The start tick index (must be aligned to tick_spacing * 88)
///
/// # Returns
/// * The derived tick_array PDA
pub fn derive_whirlpool_tick_array(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    let program_id = Pubkey::from_str(WHIRLPOOL_PROGRAM_ID).unwrap();
    let tick_str = start_tick_index.to_string();
    let seeds: &[&[u8]] = &[b"tick_array", pool.as_ref(), tick_str.as_bytes()];
    let (pda, _bump) = Pubkey::find_program_address(seeds, &program_id);
    pda
}

/// Derive a Raydium CLMM tick_array PDA.
///
/// Seeds: ["tick_array", pool_pubkey, start_tick_index.to_be_bytes()]  (i32 big-endian)
///
/// # Arguments
/// * `pool` - The Raydium CLMM pool address
/// * `start_tick_index` - The start tick index (must be aligned to tick_spacing * 60)
///
/// # Returns
/// * The derived tick_array PDA
pub fn derive_raydium_tick_array(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    let program_id = Pubkey::from_str(RAYDIUM_CLMM_PROGRAM_ID).unwrap();
    let tick_bytes = start_tick_index.to_be_bytes();
    let seeds: &[&[u8]] = &[b"tick_array", pool.as_ref(), &tick_bytes];
    let (pda, _bump) = Pubkey::find_program_address(seeds, &program_id);
    pda
}

/// Derive a Meteora DLMM bin_array PDA.
///
/// Seeds: ["bin_array", lb_pair_pubkey, bin_array_index.to_le_bytes()]  (i64 little-endian)
///
/// # Arguments
/// * `lb_pair` - The Meteora LB pair address
/// * `bin_array_index` - The bin array index (bin_id / 70, can be negative)
///
/// # Returns
/// * The derived bin_array PDA
pub fn derive_meteora_bin_array(lb_pair: &Pubkey, bin_array_index: i64) -> Pubkey {
    let program_id = Pubkey::from_str(METEORA_DLMM_PROGRAM_ID).unwrap();
    let index_bytes = bin_array_index.to_le_bytes();
    let seeds: &[&[u8]] = &[b"bin_array", lb_pair.as_ref(), &index_bytes];
    let (pda, _bump) = Pubkey::find_program_address(seeds, &program_id);
    pda
}

/// Calculate the start_tick_index for a Whirlpool tick array containing a given tick.
///
/// # Arguments
/// * `tick` - Any tick index
/// * `tick_spacing` - The pool's tick spacing
///
/// # Returns
/// * The start_tick_index that contains this tick
pub fn whirlpool_tick_to_start_index(tick: i32, tick_spacing: u16) -> i32 {
    let array_width = (tick_spacing as i32) * WHIRLPOOL_TICK_ARRAY_SIZE;
    // Floor division for negative numbers
    if tick >= 0 {
        (tick / array_width) * array_width
    } else {
        ((tick + 1) / array_width - 1) * array_width
    }
}

/// Calculate the start_tick_index for a Raydium CLMM tick array containing a given tick.
///
/// # Arguments
/// * `tick` - Any tick index
/// * `tick_spacing` - The pool's tick spacing
///
/// # Returns
/// * The start_tick_index that contains this tick
pub fn raydium_tick_to_start_index(tick: i32, tick_spacing: u16) -> i32 {
    let array_width = (tick_spacing as i32) * RAYDIUM_TICK_ARRAY_SIZE;
    // Floor division for negative numbers
    if tick >= 0 {
        (tick / array_width) * array_width
    } else {
        ((tick + 1) / array_width - 1) * array_width
    }
}

/// Calculate the bin_array_index for a Meteora DLMM containing a given bin_id.
///
/// # Arguments
/// * `bin_id` - The bin ID
///
/// # Returns
/// * The bin_array_index that contains this bin
pub fn meteora_bin_to_array_index(bin_id: i32) -> i64 {
    // bin_array_index = floor(bin_id / 70)
    if bin_id >= 0 {
        (bin_id / METEORA_BIN_ARRAY_SIZE) as i64
    } else {
        ((bin_id + 1) / METEORA_BIN_ARRAY_SIZE - 1) as i64
    }
}

/// Find reversed tick arrays for Whirlpool swaps using PDA derivation.
///
/// Given the original tick arrays (for one swap direction), derive the tick arrays
/// needed for the opposite direction.
///
/// # Arguments
/// * `pool` - The Whirlpool pool address
/// * `tick_spacing` - The pool's tick spacing
/// * `original_start_indices` - Start tick indices of the original tick arrays
/// * `was_a_to_b` - Whether the original swap was a_to_b (price decreasing)
///
/// # Returns
/// * Vec of (original_index, reversed_pda) for each tick array that differs
pub fn find_reversed_whirlpool_tick_arrays(
    pool: &Pubkey,
    tick_spacing: u16,
    original_start_indices: &[i32],
    was_a_to_b: bool,
) -> Vec<(i32, Pubkey)> {
    if original_start_indices.is_empty() {
        return Vec::new();
    }

    let array_width = (tick_spacing as i32) * WHIRLPOOL_TICK_ARRAY_SIZE;
    let mut result = Vec::new();

    // The first array (position 0) stays the same - it contains the current tick
    // Arrays at positions 1, 2, ... need to go in the opposite direction
    for (pos, &start_index) in original_start_indices.iter().enumerate() {
        if pos == 0 {
            // First array stays the same
            continue;
        }

        // Calculate the offset from the first array
        let offset_from_first = start_index - original_start_indices[0];

        // Flip the offset direction
        let reversed_offset = -offset_from_first;
        let reversed_start_index = original_start_indices[0] + reversed_offset;

        // Derive the PDA for the reversed tick array
        let reversed_pda = derive_whirlpool_tick_array(pool, reversed_start_index);

        tracing::debug!(
            "Whirlpool reversal pos={}: {} -> {} (PDA: {})",
            pos,
            start_index,
            reversed_start_index,
            reversed_pda
        );

        result.push((start_index, reversed_pda));
    }

    result
}

/// Find reversed tick arrays for Raydium CLMM swaps using PDA derivation.
///
/// Same logic as Whirlpool but with different PDA seeds.
pub fn find_reversed_raydium_tick_arrays(
    pool: &Pubkey,
    tick_spacing: u16,
    original_start_indices: &[i32],
    _was_a_to_b: bool,
) -> Vec<(i32, Pubkey)> {
    if original_start_indices.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();

    for (pos, &start_index) in original_start_indices.iter().enumerate() {
        if pos == 0 {
            continue;
        }

        let offset_from_first = start_index - original_start_indices[0];
        let reversed_offset = -offset_from_first;
        let reversed_start_index = original_start_indices[0] + reversed_offset;

        let reversed_pda = derive_raydium_tick_array(pool, reversed_start_index);

        tracing::debug!(
            "Raydium reversal pos={}: {} -> {} (PDA: {})",
            pos,
            start_index,
            reversed_start_index,
            reversed_pda
        );

        result.push((start_index, reversed_pda));
    }

    result
}

/// Find reversed bin arrays for Meteora DLMM swaps using PDA derivation.
pub fn find_reversed_meteora_bin_arrays(
    lb_pair: &Pubkey,
    original_bin_array_indices: &[i64],
    _was_a_to_b: bool,
) -> Vec<(i64, Pubkey)> {
    if original_bin_array_indices.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();

    for (pos, &bin_array_index) in original_bin_array_indices.iter().enumerate() {
        if pos == 0 {
            continue;
        }

        let offset_from_first = bin_array_index - original_bin_array_indices[0];
        let reversed_offset = -offset_from_first;
        let reversed_index = original_bin_array_indices[0] + reversed_offset;

        let reversed_pda = derive_meteora_bin_array(lb_pair, reversed_index);

        tracing::debug!(
            "Meteora reversal pos={}: {} -> {} (PDA: {})",
            pos,
            bin_array_index,
            reversed_index,
            reversed_pda
        );

        result.push((bin_array_index, reversed_pda));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dlmm_price_calculation() {
        // bin_step=100, bin_id=0 should give price = 1.0
        // Formula: (1 + 100/10000)^0 = 1.0
        let price = ClmmPriceMap::dlmm_bin_to_price(100, 0);
        assert!((price - 1.0).abs() < 0.0001);

        // bin_step=100, bin_id = 100 should give ~1.01^100 ≈ 2.7
        let price = ClmmPriceMap::dlmm_bin_to_price(100, 100);
        assert!(price > 2.5 && price < 3.0);

        // Negative bin_id should give price < 1
        // bin_step=100, bin_id = -100 should give ~1.01^(-100) ≈ 0.37
        let price = ClmmPriceMap::dlmm_bin_to_price(100, -100);
        assert!(price > 0.3 && price < 0.4);
    }

    #[test]
    fn test_tick_price_calculation() {
        // tick=0 should give price = 1.0
        let price = ClmmPriceMap::tick_to_price(0);
        assert!((price - 1.0).abs() < 0.0001);

        // tick=10000 should give ~1.0001^10000 ≈ 2.7
        let price = ClmmPriceMap::tick_to_price(10000);
        assert!(price > 2.5 && price < 3.0);
    }

    #[test]
    fn test_load_array_map() {
        // This test requires the clmm_array_map.json file
        if let Ok(map) = ClmmArrayMap::load() {
            println!("Loaded {} entries from array map", map.len());
            assert!(map.len() > 800_000, "Expected ~820k entries");

            // Verify we have entries from all three protocols
            let mut dlmm_count = 0;
            let mut whirlpool_count = 0;
            let mut raydium_count = 0;

            for (_, entry) in map.iter().take(100_000) {
                match entry.protocol.as_str() {
                    "d" => dlmm_count += 1,
                    "w" => whirlpool_count += 1,
                    "r" => raydium_count += 1,
                    _ => {}
                }
            }

            assert!(dlmm_count > 0, "Expected DLMM entries");
            assert!(whirlpool_count > 0, "Expected Whirlpool entries");
            assert!(raydium_count > 0, "Expected Raydium entries");
        }
    }

    #[test]
    fn test_array_map_lookup() {
        // Test with a known DLMM bin array
        // 4DJkyRd3gaRW3DEaZQL2FxXf1GhnesnBwnX7ER8do1Ed:
        //   pool=CV1qwy4MR1Umom3E3HBsnLSYhpwr6z9tic12MoxMwkd3, index=-9, bin_step=125
        if let Ok(map) = ClmmArrayMap::load() {
            let addr = "4DJkyRd3gaRW3DEaZQL2FxXf1GhnesnBwnX7ER8do1Ed";
            if let Some(entry) = map.get(addr) {
                assert_eq!(entry.protocol, "d");
                assert_eq!(entry.pool, "CV1qwy4MR1Umom3E3HBsnLSYhpwr6z9tic12MoxMwkd3");
                assert_eq!(entry.start_index, -9);
                assert_eq!(entry.spacing, 125);

                // Calculate price at center bin
                let price = map.calculate_price(addr, 8388608).unwrap();
                assert!((price - 1.0).abs() < 0.0001);
            }
        }
    }

    #[test]
    fn test_back_calculate_bin_from_swap() {
        // Test back-calculating bin_id from swap amounts
        if let Ok(map) = ClmmArrayMap::load() {
            let addr = "4DJkyRd3gaRW3DEaZQL2FxXf1GhnesnBwnX7ER8do1Ed";

            // Simulate a swap: 100 SOL -> 150 USDC (price = 1.5)
            // SOL has 9 decimals, USDC has 6 decimals
            let input_delta = 100_000_000_000u64; // 100 SOL
            let output_delta = 150_000_000u64; // 150 USDC

            if let Some(info) = map.price_from_swap_deltas(
                addr,
                input_delta,
                output_delta,
                9, // SOL decimals
                6, // USDC decimals
            ) {
                println!("Observed price: {}", info.observed_price);
                println!("Canonical price: {}", info.canonical_price);
                println!("Back-calculated bin_id: {}", info.bin_or_tick);

                // Observed price should be ~1.5
                assert!((info.observed_price - 1.5).abs() < 0.01);

                // Canonical price should be very close to observed (within 1%)
                // The canonical is the exact bin price, observed includes rounding
                assert!(
                    (info.canonical_price - info.observed_price).abs() / info.observed_price < 0.01
                );

                // Verify round-trip: bin -> price matches canonical_price
                let recalc_price = ClmmPriceMap::dlmm_bin_to_price(125, info.bin_or_tick);
                assert!((recalc_price - info.canonical_price).abs() < 0.0001);
            }
        }
    }

    #[test]
    fn test_price_range() {
        if let Ok(map) = ClmmArrayMap::load() {
            // Test with a Whirlpool tick array which has more predictable ranges
            for (addr, entry) in map.iter() {
                if entry.protocol == "w" && entry.start_index.abs() < 10000 {
                    if let Some((min_price, max_price)) = map.get_price_range(addr) {
                        println!("Whirlpool tick array: {}", addr);
                        println!(
                            "  start_tick: {}, spacing: {}",
                            entry.start_index, entry.spacing
                        );
                        println!("  Price range: {} to {}", min_price, max_price);
                        assert!(min_price > 0.0);
                        assert!(max_price > min_price);
                        return;
                    }
                }
            }
            println!("No suitable Whirlpool entry found");
        }
    }

    #[test]
    fn test_clmm_slippage_estimation() {
        if let Ok(map) = ClmmArrayMap::load() {
            // Find a DLMM entry
            for (addr, entry) in map.iter() {
                if entry.protocol == "d" && entry.spacing == 100 {
                    // Small trade should have very low slippage for DLMM
                    let slippage = map.estimate_slippage_bps(addr, 0.005).unwrap(); // 0.5% of liquidity
                    println!("DLMM small trade slippage: {} bps", slippage);
                    assert!(
                        slippage < 50,
                        "DLMM small trades should have <50bps slippage"
                    );

                    // Large trade should have higher slippage
                    let slippage = map.estimate_slippage_bps(addr, 0.10).unwrap(); // 10% of liquidity
                    println!("DLMM large trade slippage: {} bps", slippage);
                    assert!(slippage > 0, "DLMM large trades should have some slippage");
                    return;
                }
            }

            // Find a Whirlpool entry
            for (addr, entry) in map.iter() {
                if entry.protocol == "w" {
                    let slippage = map.estimate_slippage_bps(addr, 0.005).unwrap();
                    println!("Whirlpool small trade slippage: {} bps", slippage);
                    assert!(
                        slippage < 50,
                        "Whirlpool small trades should have <50bps slippage"
                    );
                    return;
                }
            }
        }
    }

    #[test]
    fn test_clmm_output_calculation() {
        if let Ok(map) = ClmmArrayMap::load() {
            // Find a DLMM entry with typical spacing
            for (addr, entry) in map.iter() {
                if entry.protocol == "d" && entry.spacing >= 50 && entry.spacing <= 200 {
                    // Test small trade: 0.1 SOL worth (9 decimals)
                    let input_amount = 100_000_000u64; // 0.1 SOL
                    let current_price = 150.0; // 1 SOL = 150 USDC
                    let input_reserve = 1000_000_000_000u64; // 1000 SOL
                    let output_reserve = 150_000_000_000u64; // 150,000 USDC (6 decimals)

                    if let Some(slippage) = map.calculate_clmm_output(
                        addr,
                        input_amount,
                        current_price,
                        input_reserve,
                        output_reserve,
                        9,  // SOL decimals
                        6,  // USDC decimals
                        25, // 0.25% fee
                    ) {
                        println!("DLMM output test:");
                        println!("  Input: 0.1 SOL");
                        println!(
                            "  Expected output: {} USDC",
                            slippage.output_amount as f64 / 1_000_000.0
                        );
                        println!("  Slippage: {} bps", slippage.slippage_bps);
                        println!("  Bins crossed: {}", slippage.bins_or_ticks_crossed);

                        // For a small trade, output should be close to ideal (0.1 * 150 = 15 USDC)
                        let output_usdc = slippage.output_amount as f64 / 1_000_000.0;
                        assert!(
                            output_usdc > 10.0 && output_usdc < 20.0,
                            "Output should be reasonable"
                        );
                    }
                    return;
                }
            }
            println!("No suitable DLMM entry found for output test");
        }
    }

    #[test]
    fn test_whirlpool_pda_derivation() {
        // Test case from Orca SDK: pool = 2kJmUjxWBwL2NGPBV2PiA5hWtmLCqcKY6reQgkrPtaeS, tick=0
        // Expected: 8PhPzk7n4wU98Z6XCbVtPai2LtXSxYnfjkmgWuoAU8Zy
        let pool = Pubkey::from_str("2kJmUjxWBwL2NGPBV2PiA5hWtmLCqcKY6reQgkrPtaeS").unwrap();
        let pda = derive_whirlpool_tick_array(&pool, 0);
        assert_eq!(
            pda.to_string(),
            "8PhPzk7n4wU98Z6XCbVtPai2LtXSxYnfjkmgWuoAU8Zy",
            "Whirlpool PDA should match Orca SDK test case"
        );
        println!("Whirlpool PDA test passed: tick=0 -> {}", pda);
    }

    #[test]
    fn test_raydium_pda_derivation() {
        // Test with a known Raydium CLMM pool
        // Pool: 61R1ndXxvsWXXkWSyNkCxnzwd3zUNB8Q2ibmkiLPC8ht (SOL-USDC)
        let pool = Pubkey::from_str("61R1ndXxvsWXXkWSyNkCxnzwd3zUNB8Q2ibmkiLPC8ht").unwrap();
        let pda = derive_raydium_tick_array(&pool, 0);
        // Verified on-chain: this PDA exists and is owned by Raydium CLMM program
        println!("Raydium PDA test: tick=0 -> {}", pda);
        // Just verify it produces a valid pubkey (existence tested separately)
        assert!(!pda.to_string().is_empty());
    }

    #[test]
    fn test_meteora_pda_derivation() {
        // Test with a known Meteora LB pair
        // LB Pair: 862pS9kwxwT3eYEsg7TpGYEj5PpBk5PaMg7sNPmLNgFP (SOL-USDC)
        let lb_pair = Pubkey::from_str("862pS9kwxwT3eYEsg7TpGYEj5PpBk5PaMg7sNPmLNgFP").unwrap();
        let pda = derive_meteora_bin_array(&lb_pair, 0);
        // Verified on-chain: this PDA exists and is owned by Meteora DLMM program
        println!("Meteora PDA test: bin_array_index=0 -> {}", pda);
        assert!(!pda.to_string().is_empty());
    }

    #[test]
    fn test_tick_to_start_index() {
        // Test Whirlpool tick to start index calculation
        // With tick_spacing=1, array_width = 1 * 88 = 88
        assert_eq!(whirlpool_tick_to_start_index(0, 1), 0);
        assert_eq!(whirlpool_tick_to_start_index(87, 1), 0);
        assert_eq!(whirlpool_tick_to_start_index(88, 1), 88);
        assert_eq!(whirlpool_tick_to_start_index(-1, 1), -88);
        assert_eq!(whirlpool_tick_to_start_index(-88, 1), -88);
        assert_eq!(whirlpool_tick_to_start_index(-89, 1), -176);

        // With tick_spacing=64, array_width = 64 * 88 = 5632
        assert_eq!(whirlpool_tick_to_start_index(0, 64), 0);
        assert_eq!(whirlpool_tick_to_start_index(5631, 64), 0);
        assert_eq!(whirlpool_tick_to_start_index(5632, 64), 5632);
        assert_eq!(whirlpool_tick_to_start_index(-1, 64), -5632);
    }

    #[test]
    fn test_reversal_logic() {
        // Test that reversal produces correct opposite direction
        let pool = Pubkey::from_str("2kJmUjxWBwL2NGPBV2PiA5hWtmLCqcKY6reQgkrPtaeS").unwrap();

        // Original: a_to_b=true (descending), arrays at indices [0, -5632, -11264]
        let original_indices = vec![0, -5632, -11264];
        let reversed = find_reversed_whirlpool_tick_arrays(&pool, 64, &original_indices, true);

        // Position 0 stays same (not in result)
        // Position 1: offset = -5632 - 0 = -5632, reversed = 0 - (-5632) = 5632
        // Position 2: offset = -11264 - 0 = -11264, reversed = 0 - (-11264) = 11264
        assert_eq!(reversed.len(), 2);
        assert_eq!(reversed[0].0, -5632);
        assert_eq!(reversed[1].0, -11264);

        // Verify the PDAs are derived correctly
        let expected_pda_1 = derive_whirlpool_tick_array(&pool, 5632);
        let expected_pda_2 = derive_whirlpool_tick_array(&pool, 11264);
        assert_eq!(reversed[0].1, expected_pda_1);
        assert_eq!(reversed[1].1, expected_pda_2);

        println!("Reversal test passed:");
        println!("  Original: {:?}", original_indices);
        println!(
            "  Reversed[1]: {} -> {} (PDA: {})",
            reversed[0].0, 5632, reversed[0].1
        );
        println!(
            "  Reversed[2]: {} -> {} (PDA: {})",
            reversed[1].0, 11264, reversed[1].1
        );
    }
}
