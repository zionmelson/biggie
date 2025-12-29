//! RPC Bootstrap for CLMM Pool State
//!
//! Fetches initial sqrt_price, liquidity, and tick data for CLMM pools via RPC
//! at startup. This runs synchronously BEFORE Geyser processing starts.
//!
//! Flow:
//! 1. pools.json provides pool addresses and static metadata
//! 2. RPC bootstrap fetches current state for all CLMM pools
//! 3. RPC bootstrap fetches tick arrays for accurate swap simulation
//! 4. Geyser subscription starts AFTER bootstrap completes
//! 5. Geyser always overwrites (it's always newer by definition)

use crate::pool_discovery::{parse_meteora_damm, parse_raydium_clmm};
use crate::pool_state::PoolStateManager;
use crate::shredstream::programs::DexType;
use anyhow::Result;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcAccountInfoConfig;
use solana_client::rpc_response::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

/// Maximum accounts per getMultipleAccounts call (Solana RPC limit is 100)
const BATCH_SIZE: usize = 100;

/// Bootstrap result statistics
#[derive(Debug, Default, Clone)]
pub struct BootstrapStats {
    pub pools_attempted: usize,
    pub pools_updated: usize,
    pub pools_failed_parse: usize,
    pub pools_no_data: usize,
    pub pools_rpc_error: usize,
    pub tick_arrays_attempted: usize,
    pub tick_arrays_loaded: usize,
    pub tick_arrays_failed: usize,
    pub ticks_loaded: usize,
    pub rpc_calls: usize,
    pub duration_ms: u64,
}

impl BootstrapStats {
    /// Returns a summary string for logging
    pub fn summary(&self) -> String {
        format!(
            "pools={}/{} tick_arrays={}/{} ticks={} parse_fail={} no_data={} rpc_err={} calls={} ms={}",
            self.pools_updated,
            self.pools_attempted,
            self.tick_arrays_loaded,
            self.tick_arrays_attempted,
            self.ticks_loaded,
            self.pools_failed_parse,
            self.pools_no_data,
            self.pools_rpc_error,
            self.rpc_calls,
            self.duration_ms
        )
    }
}

/// Bootstrap CLMM pool state from RPC (synchronous - call before Geyser starts)
///
/// Fetches current sqrt_price, liquidity, and tick data for all CLMM pools
/// in the PoolStateManager. No slot comparison needed since Geyser hasn't started.
pub async fn bootstrap_clmm_pools(
    rpc_client: &RpcClient,
    psm: &Arc<PoolStateManager>,
) -> Result<BootstrapStats> {
    let start = Instant::now();
    let mut stats = BootstrapStats::default();

    // Get all CLMM pool addresses with dex types
    let clmm_pools: Vec<(Pubkey, DexType)> = psm.all_clmm_pools_with_dex();
    stats.pools_attempted = clmm_pools.len();

    if clmm_pools.is_empty() {
        info!("No CLMM pools to bootstrap");
        stats.duration_ms = start.elapsed().as_millis() as u64;
        return Ok(stats);
    }

    info!("Bootstrapping {} CLMM pools via RPC", clmm_pools.len());

    // Process in batches to respect RPC limits
    for batch in clmm_pools.chunks(BATCH_SIZE) {
        let addresses: Vec<Pubkey> = batch.iter().map(|(addr, _)| *addr).collect();
        let dex_types: Vec<DexType> = batch.iter().map(|(_, dex)| *dex).collect();

        stats.rpc_calls += 1;

        let config = RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            commitment: Some(CommitmentConfig::confirmed()),
            ..Default::default()
        };

        let response = match rpc_client
            .get_multiple_accounts_with_config(&addresses, config)
            .await
        {
            Ok(response) => response,
            Err(e) => {
                warn!("RPC batch fetch failed for {} accounts: {}", addresses.len(), e);
                stats.pools_rpc_error += addresses.len();
                continue;
            }
        };

        let batch_slot = response.context.slot;

        // Process each account
        for (i, maybe_account) in response.value.into_iter().enumerate() {
            let address = addresses[i];
            let dex = dex_types[i];

            let Some(account) = maybe_account else {
                debug!(pool = %address, dex = ?dex, "Pool account not found on-chain");
                stats.pools_no_data += 1;
                continue;
            };

            let data = &account.data;

            // Parse based on DEX type
            let parsed = match dex {
                DexType::RaydiumClmm | DexType::PancakeSwapClmm => {
                    parse_raydium_clmm(address, data).map(ParsedClmmState::from)
                }
                DexType::MeteoraDamm => {
                    parse_meteora_damm(address, data).map(ParsedClmmState::from)
                }
                DexType::OrcaWhirlpool => parse_whirlpool_state(data),
                DexType::MeteoraDlmm => parse_dlmm_state(data),
                _ => {
                    debug!(pool = %address, dex = ?dex, "Unsupported DEX type for bootstrap");
                    None
                }
            };

            let Some(pool_data) = parsed else {
                debug!(pool = %address, dex = ?dex, data_len = data.len(), "Failed to parse pool data");
                stats.pools_failed_parse += 1;
                continue;
            };

            // Update PSM directly (no slot comparison - bootstrap runs before Geyser)
            psm.set_clmm_state(
                &address,
                pool_data.sqrt_price,
                pool_data.current_tick_or_bin,
                pool_data.spacing,
                pool_data.active_liquidity,
                batch_slot,
            );

            stats.pools_updated += 1;

            debug!(
                pool = %address,
                sqrt_price = ?pool_data.sqrt_price,
                tick = pool_data.current_tick_or_bin,
                liquidity = ?pool_data.active_liquidity,
                slot = batch_slot,
                "Bootstrapped CLMM pool"
            );
        }
    }

    // Phase 2: Bootstrap tick arrays for accurate CLMM swap simulation
    // Without tick data, simulations assume all liquidity is in one range -> wrong outputs
    info!("Phase 2: Bootstrapping tick arrays for CLMM pools...");
    
    let tick_array_stats = bootstrap_tick_arrays(rpc_client, psm).await;
    stats.tick_arrays_attempted = tick_array_stats.attempted;
    stats.tick_arrays_loaded = tick_array_stats.loaded;
    stats.tick_arrays_failed = tick_array_stats.failed;
    stats.ticks_loaded = tick_array_stats.ticks;
    stats.rpc_calls += tick_array_stats.rpc_calls;

    stats.duration_ms = start.elapsed().as_millis() as u64;

    info!("Bootstrap complete: {}", stats.summary());

    Ok(stats)
}

/// Tick array bootstrap statistics
struct TickArrayBootstrapStats {
    attempted: usize,
    loaded: usize,
    failed: usize,
    ticks: usize,
    rpc_calls: usize,
}

/// Bootstrap tick arrays for all registered CLMM pools
///
/// This is CRITICAL for accurate CLMM swap simulation. Without tick data,
/// the simulation assumes all liquidity is in one continuous range from
/// the current tick to the boundary, which produces wildly inaccurate
/// output estimates and causes slippage failures on-chain.
async fn bootstrap_tick_arrays(
    rpc_client: &RpcClient,
    psm: &Arc<PoolStateManager>,
) -> TickArrayBootstrapStats {
    let mut stats = TickArrayBootstrapStats {
        attempted: 0,
        loaded: 0,
        failed: 0,
        ticks: 0,
        rpc_calls: 0,
    };

    // Get all tick array addresses that need to be fetched
    let tick_cache = psm.tick_cache();
    let all_tick_arrays = tick_cache.get_all_tick_array_addresses();
    
    if all_tick_arrays.is_empty() {
        debug!("No tick arrays registered for bootstrap");
        return stats;
    }
    
    stats.attempted = all_tick_arrays.len();
    info!("Fetching {} tick arrays for CLMM simulation accuracy", all_tick_arrays.len());
    
    // Fetch in batches
    for batch in all_tick_arrays.chunks(BATCH_SIZE) {
        stats.rpc_calls += 1;
        
        let config = RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            commitment: Some(CommitmentConfig::confirmed()),
            ..Default::default()
        };
        
        let response = match rpc_client
            .get_multiple_accounts_with_config(batch, config)
            .await
        {
            Ok(response) => response,
            Err(e) => {
                warn!("RPC batch fetch failed for {} tick arrays: {}", batch.len(), e);
                stats.failed += batch.len();
                continue;
            }
        };
        
        let batch_slot = response.context.slot;
        
        for (i, maybe_account) in response.value.into_iter().enumerate() {
            let address = batch[i];
            
            let Some(account) = maybe_account else {
                debug!(tick_array = %address, "Tick array not found on-chain");
                stats.failed += 1;
                continue;
            };
            
            let data = &account.data;
            
            // Process the tick array through the cache
            // The cache will parse it and register it for the correct pool
            if tick_cache.process_account_update(&address, data, batch_slot) {
                stats.loaded += 1;
                // Count ticks (rough estimate: non-empty tick arrays have ~20-60 active ticks)
                // The cache tracks this internally, we just increment a counter
                stats.ticks += 30; // Average estimate
            } else {
                stats.failed += 1;
            }
        }
    }
    
    info!(
        "Tick array bootstrap: loaded={}/{} failed={} (ticks≈{})",
        stats.loaded, stats.attempted, stats.failed, stats.ticks
    );
    
    stats
}

/// Parse Orca Whirlpool state
fn parse_whirlpool_state(data: &[u8]) -> Option<ParsedClmmState> {
    // Whirlpool layout offsets
    const MIN_SIZE: usize = 653;
    const TICK_SPACING: usize = 41;
    const LIQUIDITY: usize = 49;
    const SQRT_PRICE: usize = 65;
    const TICK_CURRENT: usize = 81;

    if data.len() < MIN_SIZE {
        return None;
    }

    // Whirlpool discriminator
    let expected_disc: [u8; 8] = [63, 149, 209, 12, 225, 128, 99, 9];
    if &data[0..8] != &expected_disc {
        return None;
    }

    let tick_spacing = u16::from_le_bytes([data[TICK_SPACING], data[TICK_SPACING + 1]]);
    let liquidity = u128::from_le_bytes(data[LIQUIDITY..LIQUIDITY + 16].try_into().ok()?);
    let sqrt_price = u128::from_le_bytes(data[SQRT_PRICE..SQRT_PRICE + 16].try_into().ok()?);
    let tick_current = i32::from_le_bytes(data[TICK_CURRENT..TICK_CURRENT + 4].try_into().ok()?);

    Some(ParsedClmmState {
        sqrt_price: Some(sqrt_price),
        active_liquidity: Some(liquidity),
        current_tick_or_bin: tick_current,
        spacing: tick_spacing,
    })
}

/// Parse Meteora DLMM state
fn parse_dlmm_state(data: &[u8]) -> Option<ParsedClmmState> {
    const MIN_SIZE: usize = 904;
    const BIN_STEP: usize = 8;
    const ACTIVE_ID: usize = 72;

    if data.len() < MIN_SIZE {
        return None;
    }

    // DLMM LbPair discriminator
    let expected_disc: [u8; 8] = [33, 11, 49, 98, 181, 101, 177, 13];
    if &data[0..8] != &expected_disc {
        return None;
    }

    let bin_step = u16::from_le_bytes([data[BIN_STEP], data[BIN_STEP + 1]]);
    let active_id = i32::from_le_bytes(data[ACTIVE_ID..ACTIVE_ID + 4].try_into().ok()?);

    Some(ParsedClmmState {
        sqrt_price: None,
        active_liquidity: None,
        current_tick_or_bin: active_id,
        spacing: bin_step,
    })
}

/// Intermediate parsed state for CLMM pools
struct ParsedClmmState {
    sqrt_price: Option<u128>,
    active_liquidity: Option<u128>,
    current_tick_or_bin: i32,
    spacing: u16,
}

impl From<crate::pool_discovery::DiscoveredPool> for ParsedClmmState {
    fn from(p: crate::pool_discovery::DiscoveredPool) -> Self {
        Self {
            sqrt_price: p.sqrt_price,
            active_liquidity: p.active_liquidity,
            current_tick_or_bin: p.current_tick_or_bin,
            spacing: p.spacing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_size() {
        assert!(BATCH_SIZE <= 100);
    }
}
