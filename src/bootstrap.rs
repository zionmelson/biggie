//! RPC Bootstrap for CLMM Pool State
//!
//! Fetches initial sqrt_price, liquidity, and tick data for CLMM pools via RPC
//! at startup. This runs synchronously BEFORE Geyser processing starts.
//!
//! Flow:
//! 1. pools.json provides pool addresses and static metadata
//! 2. RPC bootstrap fetches current state for all CLMM pools
//! 3. Geyser subscription starts AFTER bootstrap completes
//! 4. Geyser always overwrites (it's always newer by definition)

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
    pub rpc_calls: usize,
    pub duration_ms: u64,
}

impl BootstrapStats {
    /// Returns a summary string for logging
    pub fn summary(&self) -> String {
        format!(
            "updated={}/{} parse_fail={} no_data={} rpc_err={} calls={} ms={}",
            self.pools_updated,
            self.pools_attempted,
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

    stats.duration_ms = start.elapsed().as_millis() as u64;

    info!("Bootstrap complete: {}", stats.summary());

    Ok(stats)
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
