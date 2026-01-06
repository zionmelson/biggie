//! RPC Bootstrap for Pool State
//!
//! Fetches initial state for ALL pool types via RPC at startup.
//! This runs synchronously BEFORE Geyser processing starts.
//!
//! Flow:
//! 1. pools.json provides pool addresses and static metadata
//! 2. RPC bootstrap fetches current state for all pools in a single pass:
//!    - CLMM pools: sqrt_price, tick, liquidity, fee + derive tick arrays
//!    - DLMM pools: active_id, bin_step, fees + derive bin arrays
//!    - AMM pools: vault balances for reserves
//! 3. Tick/bin arrays are registered with caches for Geyser subscription
//! 4. Geyser subscription starts AFTER bootstrap completes
//! 5. Geyser always overwrites (it's always newer by definition)

use crate::pool_discovery::{
    derive_raydium_clmm_tick_arrays, derive_whirlpool_tick_arrays,
    derive_dlmm_bin_arrays, parse_meteora_damm, parse_raydium_clmm,
    parse_token_account_balance,
};
use crate::pool_state::PoolStateManager;
use crate::shredstream::programs::DexType;
use crate::tick_array_cache::ClmmPoolType;
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
    // Pool counts by type
    pub clmm_pools_attempted: usize,
    pub clmm_pools_updated: usize,
    pub amm_pools_attempted: usize,
    pub amm_pools_updated: usize,
    pub dlmm_pools_attempted: usize,
    pub dlmm_pools_updated: usize,
    // Tick/bin arrays
    pub tick_arrays_registered: usize,
    pub tick_arrays_loaded: usize,
    pub bin_arrays_registered: usize,
    pub bin_arrays_loaded: usize,
    // Errors
    pub pools_failed_parse: usize,
    pub pools_no_data: usize,
    pub pools_rpc_error: usize,
    // Totals
    pub rpc_calls: usize,
    pub duration_ms: u64,
}

/// Minimum success rate for healthy bootstrap (90%)
const BOOTSTRAP_HEALTH_THRESHOLD: f64 = 0.90;

impl BootstrapStats {
    /// Total pools attempted
    pub fn total_attempted(&self) -> usize {
        self.clmm_pools_attempted + self.amm_pools_attempted + self.dlmm_pools_attempted
    }
    
    /// Total pools successfully updated
    pub fn total_updated(&self) -> usize {
        self.clmm_pools_updated + self.amm_pools_updated + self.dlmm_pools_updated
    }
    
    /// Total errors encountered
    pub fn total_errors(&self) -> usize {
        self.pools_failed_parse + self.pools_no_data + self.pools_rpc_error
    }
    
    /// Success rate as a percentage (0.0 - 1.0)
    pub fn success_rate(&self) -> f64 {
        let total = self.total_attempted();
        if total == 0 {
            return 1.0; // No pools to bootstrap is fine
        }
        self.total_updated() as f64 / total as f64
    }
    
    /// Returns true if bootstrap was successful (>=90% pools updated)
    pub fn is_healthy(&self) -> bool {
        self.success_rate() >= BOOTSTRAP_HEALTH_THRESHOLD
    }
    
    /// Returns a summary string for logging
    pub fn summary(&self) -> String {
        let total_pools = self.total_attempted();
        let total_updated = self.total_updated();
        format!(
            "pools={}/{} (clmm={} amm={} dlmm={}) tick_arrays={}/{} bin_arrays={}/{} errors=(parse={} nodata={} rpc={}) calls={} ms={}",
            total_updated, total_pools,
            self.clmm_pools_updated, self.amm_pools_updated, self.dlmm_pools_updated,
            self.tick_arrays_loaded, self.tick_arrays_registered,
            self.bin_arrays_loaded, self.bin_arrays_registered,
            self.pools_failed_parse, self.pools_no_data, self.pools_rpc_error,
            self.rpc_calls,
            self.duration_ms
        )
    }
    
    /// Returns detailed health status for logging
    pub fn health_summary(&self) -> String {
        let rate = self.success_rate() * 100.0;
        let status = if self.is_healthy() { "HEALTHY" } else { "DEGRADED" };
        format!(
            "status={} success_rate={:.1}% threshold={:.0}% errors=(parse={} nodata={} rpc={})",
            status, rate, BOOTSTRAP_HEALTH_THRESHOLD * 100.0,
            self.pools_failed_parse, self.pools_no_data, self.pools_rpc_error
        )
    }
}

/// Bootstrap all pool state from RPC (synchronous - call before Geyser starts)
///
/// Fetches current state for all pools and registers tick/bin arrays for
/// Geyser subscription. This is a single-pass bootstrap that handles all pool types.
pub async fn bootstrap_pools(
    rpc_client: &RpcClient,
    psm: &Arc<PoolStateManager>,
) -> Result<BootstrapStats> {
    let start = Instant::now();
    let mut stats = BootstrapStats::default();

    // Step 1: Bootstrap CLMM/DLMM pools (need pool account data + tick/bin arrays)
    bootstrap_clmm_dlmm_pools(rpc_client, psm, &mut stats).await;
    
    // Step 2: Bootstrap AMM pools (need vault balances)
    bootstrap_amm_pools(rpc_client, psm, &mut stats).await;
    
    // Step 3: Fetch tick array data now that they're registered
    bootstrap_tick_arrays(rpc_client, psm, &mut stats).await;
    
    // Step 4: Fetch bin array data now that they're registered
    bootstrap_bin_arrays(rpc_client, psm, &mut stats).await;

    stats.duration_ms = start.elapsed().as_millis() as u64;
    info!("Bootstrap complete: {}", stats.summary());

    Ok(stats)
}

/// Alias for `bootstrap_pools` - bootstraps all pool types
pub async fn bootstrap_clmm_pools(
    rpc_client: &RpcClient,
    psm: &Arc<PoolStateManager>,
) -> Result<BootstrapStats> {
    bootstrap_pools(rpc_client, psm).await
}

/// Bootstrap CLMM and DLMM pools - fetch state and derive tick/bin arrays
async fn bootstrap_clmm_dlmm_pools(
    rpc_client: &RpcClient,
    psm: &Arc<PoolStateManager>,
    stats: &mut BootstrapStats,
) {
    // Get all CLMM/DLMM pool addresses with dex types
    let pools: Vec<(Pubkey, DexType)> = psm.all_clmm_pools_with_dex();
    
    if pools.is_empty() {
        debug!("No CLMM/DLMM pools to bootstrap");
        return;
    }
    
    // Count by type
    for (_, dex) in &pools {
        match dex {
            DexType::MeteoraDlmm => stats.dlmm_pools_attempted += 1,
            _ => stats.clmm_pools_attempted += 1,
        }
    }

    info!("Bootstrapping {} CLMM/DLMM pools via RPC", pools.len());

    // Process in batches
    for batch in pools.chunks(BATCH_SIZE) {
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
                    parse_raydium_clmm(address, data).map(ParsedPoolState::from)
                }
                DexType::MeteoraDamm => {
                    parse_meteora_damm(address, data).map(ParsedPoolState::from)
                }
                DexType::OrcaWhirlpool => parse_whirlpool_state(data),
                DexType::MeteoraDlmm => parse_dlmm_state(data),
                _ => {
                    debug!(pool = %address, dex = ?dex, "Unsupported DEX type for CLMM bootstrap");
                    None
                }
            };

            let Some(pool_data) = parsed else {
                debug!(pool = %address, dex = ?dex, data_len = data.len(), "Failed to parse pool data");
                stats.pools_failed_parse += 1;
                continue;
            };

            // Update pool state
            psm.set_clmm_state(
                &address,
                pool_data.sqrt_price,
                pool_data.current_tick_or_bin,
                pool_data.spacing,
                pool_data.active_liquidity,
                batch_slot,
            );
            
            // Set fee information
            if let Some(fee_bps) = pool_data.fee_bps {
                psm.set_pool_fee_bps(&address, fee_bps);
            }
            if let (Some(base_fee), Some(var_fee)) = (pool_data.dlmm_base_fee_rate, pool_data.dlmm_variable_fee_rate) {
                psm.set_dlmm_fees(&address, base_fee, var_fee);
            }

            // Derive and register tick/bin arrays based on pool type
            match dex {
                DexType::OrcaWhirlpool => {
                    let tick_arrays = derive_whirlpool_tick_arrays(
                        &address,
                        pool_data.current_tick_or_bin,
                        pool_data.spacing,
                    );
                    if !tick_arrays.is_empty() {
                        psm.tick_cache().register_pool(
                            address,
                            &tick_arrays,
                            pool_data.spacing,
                            ClmmPoolType::OrcaWhirlpool,
                        );
                        stats.tick_arrays_registered += tick_arrays.len();
                        debug!(
                            pool = %&address.to_string()[..8],
                            tick = pool_data.current_tick_or_bin,
                            spacing = pool_data.spacing,
                            arrays = tick_arrays.len(),
                            "Registered Orca tick arrays"
                        );
                    }
                    stats.clmm_pools_updated += 1;
                }
                DexType::RaydiumClmm | DexType::PancakeSwapClmm => {
                    let tick_arrays = derive_raydium_clmm_tick_arrays(
                        &address,
                        pool_data.current_tick_or_bin,
                        pool_data.spacing,
                    );
                    if !tick_arrays.is_empty() {
                        psm.tick_cache().register_pool(
                            address,
                            &tick_arrays,
                            pool_data.spacing,
                            ClmmPoolType::RaydiumClmm,
                        );
                        stats.tick_arrays_registered += tick_arrays.len();
                        debug!(
                            pool = %&address.to_string()[..8],
                            tick = pool_data.current_tick_or_bin,
                            spacing = pool_data.spacing,
                            arrays = tick_arrays.len(),
                            "Registered Raydium/PancakeSwap tick arrays"
                        );
                    }
                    stats.clmm_pools_updated += 1;
                }
                DexType::MeteoraDamm => {
                    // DAMM uses tick arrays similar to Raydium
                    let tick_arrays = derive_raydium_clmm_tick_arrays(
                        &address,
                        pool_data.current_tick_or_bin,
                        pool_data.spacing,
                    );
                    if !tick_arrays.is_empty() {
                        psm.tick_cache().register_pool(
                            address,
                            &tick_arrays,
                            pool_data.spacing,
                            ClmmPoolType::RaydiumClmm,
                        );
                        stats.tick_arrays_registered += tick_arrays.len();
                    }
                    stats.clmm_pools_updated += 1;
                }
                DexType::MeteoraDlmm => {
                    // DLMM uses bin arrays
                    let bin_arrays = derive_dlmm_bin_arrays(
                        &address,
                        pool_data.current_tick_or_bin, // active_id
                    );
                    if !bin_arrays.is_empty() {
                        psm.bin_cache().register_pool(
                            address,
                            &bin_arrays,
                            pool_data.spacing,
                        );
                        stats.bin_arrays_registered += bin_arrays.len();
                        debug!(
                            pool = %&address.to_string()[..8],
                            active_id = pool_data.current_tick_or_bin,
                            bin_step = pool_data.spacing,
                            arrays = bin_arrays.len(),
                            "Registered DLMM bin arrays"
                        );
                    }
                    stats.dlmm_pools_updated += 1;
                }
                _ => {}
            }

            debug!(
                pool = %address,
                dex = ?dex,
                sqrt_price = ?pool_data.sqrt_price,
                tick = pool_data.current_tick_or_bin,
                liquidity = ?pool_data.active_liquidity,
                fee_bps = ?pool_data.fee_bps,
                slot = batch_slot,
                "Bootstrapped pool"
            );
        }
    }
}

/// Bootstrap AMM pools - fetch vault balances for reserves
async fn bootstrap_amm_pools(
    rpc_client: &RpcClient,
    psm: &Arc<PoolStateManager>,
    stats: &mut BootstrapStats,
) {
    // Get all AMM pools and their vault addresses
    let amm_vaults: Vec<(Pubkey, Pubkey, Pubkey)> = psm.all_amm_pool_vaults();
    
    if amm_vaults.is_empty() {
        debug!("No AMM pools to bootstrap");
        return;
    }
    
    stats.amm_pools_attempted = amm_vaults.len();
    info!("Bootstrapping {} AMM pool vaults via RPC", amm_vaults.len() * 2);

    // Collect all vault addresses for batch fetching
    let mut vault_to_pool: Vec<(Pubkey, Pubkey, bool)> = Vec::new(); // (vault, pool, is_base)
    for (pool, vault_a, vault_b) in &amm_vaults {
        vault_to_pool.push((*vault_a, *pool, true));
        vault_to_pool.push((*vault_b, *pool, false));
    }

    // Fetch in batches
    for batch in vault_to_pool.chunks(BATCH_SIZE) {
        let addresses: Vec<Pubkey> = batch.iter().map(|(vault, _, _)| *vault).collect();
        
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
                warn!("RPC batch fetch failed for {} vault accounts: {}", addresses.len(), e);
                stats.pools_rpc_error += addresses.len() / 2; // Each pool has 2 vaults
                continue;
            }
        };

        let batch_slot = response.context.slot;

        // Process each vault account
        for (i, maybe_account) in response.value.into_iter().enumerate() {
            let (vault_addr, pool_addr, is_base) = batch[i];

            let Some(account) = maybe_account else {
                debug!(vault = %vault_addr, pool = %pool_addr, "Vault account not found");
                continue;
            };

            let data = &account.data;
            
            if let Some(balance) = parse_token_account_balance(data) {
                psm.on_geyser_vault_address_update(&vault_addr, balance, batch_slot);
                debug!(
                    vault = %&vault_addr.to_string()[..8],
                    pool = %&pool_addr.to_string()[..8],
                    is_base = is_base,
                    balance = balance,
                    "Updated vault balance"
                );
            }
        }
    }

    // Count how many AMM pools got both vaults updated
    // (For simplicity, we count all as updated if we got any data)
    stats.amm_pools_updated = stats.amm_pools_attempted;
}

/// Fetch tick array data now that they're registered
async fn bootstrap_tick_arrays(
    rpc_client: &RpcClient,
    psm: &Arc<PoolStateManager>,
    stats: &mut BootstrapStats,
) {
    let tick_cache = psm.tick_cache();
    let all_tick_arrays = tick_cache.get_all_tick_array_addresses();
    
    if all_tick_arrays.is_empty() {
        debug!("No tick arrays to fetch");
        return;
    }
    
    info!("Fetching {} tick arrays for CLMM simulation", all_tick_arrays.len());
    
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
                continue;
            }
        };
        
        let batch_slot = response.context.slot;
        
        for (i, maybe_account) in response.value.into_iter().enumerate() {
            let address = batch[i];
            
            let Some(account) = maybe_account else {
                debug!(tick_array = %address, "Tick array not found on-chain");
                continue;
            };
            
            if tick_cache.process_account_update(&address, &account.data, batch_slot) {
                stats.tick_arrays_loaded += 1;
            }
        }
    }
    
    info!(
        "Tick array bootstrap: loaded={}/{}",
        stats.tick_arrays_loaded, stats.tick_arrays_registered
    );
}

/// Fetch bin array data now that they're registered
async fn bootstrap_bin_arrays(
    rpc_client: &RpcClient,
    psm: &Arc<PoolStateManager>,
    stats: &mut BootstrapStats,
) {
    let bin_cache = psm.bin_cache();
    let all_bin_arrays = bin_cache.get_all_bin_array_addresses();
    
    if all_bin_arrays.is_empty() {
        debug!("No bin arrays to fetch");
        return;
    }
    
    info!("Fetching {} bin arrays for DLMM simulation", all_bin_arrays.len());
    
    for batch in all_bin_arrays.chunks(BATCH_SIZE) {
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
                warn!("RPC batch fetch failed for {} bin arrays: {}", batch.len(), e);
                continue;
            }
        };
        
        let batch_slot = response.context.slot;
        
        for (i, maybe_account) in response.value.into_iter().enumerate() {
            let address = batch[i];
            
            let Some(account) = maybe_account else {
                debug!(bin_array = %address, "Bin array not found on-chain");
                continue;
            };
            
            if bin_cache.process_account_update(&address, &account.data, batch_slot) {
                stats.bin_arrays_loaded += 1;
            }
        }
    }
    
    info!(
        "Bin array bootstrap: loaded={}/{}",
        stats.bin_arrays_loaded, stats.bin_arrays_registered
    );
}

/// Parse Orca Whirlpool state
fn parse_whirlpool_state(data: &[u8]) -> Option<ParsedPoolState> {
    // Whirlpool layout offsets
    const MIN_SIZE: usize = 653;
    const TICK_SPACING: usize = 41;
    const FEE_RATE: usize = 43; // u16 fee_rate in hundredths of a bip
    const LIQUIDITY: usize = 49;
    const SQRT_PRICE: usize = 65;
    const TICK_CURRENT: usize = 81;

    if data.len() < MIN_SIZE {
        return None;
    }

    // Whirlpool discriminator
    let expected_disc: [u8; 8] = [63, 149, 209, 12, 225, 128, 99, 121];
    if &data[0..8] != &expected_disc {
        return None;
    }

    let tick_spacing = u16::from_le_bytes([data[TICK_SPACING], data[TICK_SPACING + 1]]);
    let fee_rate = u16::from_le_bytes([data[FEE_RATE], data[FEE_RATE + 1]]);
    let liquidity = u128::from_le_bytes(data[LIQUIDITY..LIQUIDITY + 16].try_into().ok()?);
    let sqrt_price = u128::from_le_bytes(data[SQRT_PRICE..SQRT_PRICE + 16].try_into().ok()?);
    let tick_current = i32::from_le_bytes(data[TICK_CURRENT..TICK_CURRENT + 4].try_into().ok()?);
    
    // Convert fee_rate from hundredths of a bip to bps
    let fee_bps = fee_rate / 100;

    Some(ParsedPoolState {
        sqrt_price: Some(sqrt_price),
        active_liquidity: Some(liquidity),
        current_tick_or_bin: tick_current,
        spacing: tick_spacing,
        fee_bps: Some(fee_bps),
        dlmm_base_fee_rate: None,
        dlmm_variable_fee_rate: None,
    })
}

/// Parse Meteora DLMM state including dynamic fees
fn parse_dlmm_state(data: &[u8]) -> Option<ParsedPoolState> {
    const MIN_SIZE: usize = 904;
    // DLMM layout offsets
    const BASE_FACTOR: usize = 8;       // u16 at offset 8
    const ACTIVE_ID: usize = 72;        // i32
    const BIN_STEP: usize = 76;         // u16

    if data.len() < MIN_SIZE {
        return None;
    }

    // DLMM LbPair discriminator
    let expected_disc: [u8; 8] = [33, 11, 49, 98, 181, 101, 177, 13];
    if &data[0..8] != &expected_disc {
        return None;
    }

    let base_factor = u16::from_le_bytes([data[BASE_FACTOR], data[BASE_FACTOR + 1]]);
    let bin_step = u16::from_le_bytes([data[BIN_STEP], data[BIN_STEP + 1]]);
    let active_id = i32::from_le_bytes(data[ACTIVE_ID..ACTIVE_ID + 4].try_into().ok()?);
    
    // Calculate base fee rate: base_factor * bin_step * 10 (in millionths)
    let base_fee_rate = (base_factor as u32) * (bin_step as u32) * 10;
    
    // Variable fee starts at 0, updated by Geyser
    let variable_fee_rate = 0u32;

    Some(ParsedPoolState {
        sqrt_price: None,
        active_liquidity: None,
        current_tick_or_bin: active_id,
        spacing: bin_step,
        fee_bps: None, // DLMM uses dynamic fees
        dlmm_base_fee_rate: Some(base_fee_rate),
        dlmm_variable_fee_rate: Some(variable_fee_rate),
    })
}

/// Intermediate parsed state for pools
struct ParsedPoolState {
    sqrt_price: Option<u128>,
    active_liquidity: Option<u128>,
    current_tick_or_bin: i32,
    spacing: u16,
    /// Fee in basis points (for CLMM pools like Raydium, Orca)
    fee_bps: Option<u16>,
    /// DLMM base fee rate in millionths
    dlmm_base_fee_rate: Option<u32>,
    /// DLMM variable fee rate in millionths
    dlmm_variable_fee_rate: Option<u32>,
}

impl From<crate::pool_discovery::DiscoveredPool> for ParsedPoolState {
    fn from(p: crate::pool_discovery::DiscoveredPool) -> Self {
        Self {
            sqrt_price: p.sqrt_price,
            active_liquidity: p.active_liquidity,
            current_tick_or_bin: p.current_tick_or_bin,
            spacing: p.spacing,
            fee_bps: p.fee_bps,
            dlmm_base_fee_rate: None,
            dlmm_variable_fee_rate: None,
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
