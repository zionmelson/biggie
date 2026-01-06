//! Bootstrap - Initialize CLMM pools with tick array discovery
//!
//! Fetches on-chain data to populate amm_config, observation_state, current_tick,
//! and discovers which tick arrays exist for each CLMM pool.

use crate::pools::{DexType, Pool, PoolRegistry, TickArrayInfo, derive_tick_array_pda, all_tick_array_starts};
use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use tracing::{info, debug, warn};

/// Pool state layout offsets for Raydium CLMM
const POOL_AMM_CONFIG_OFFSET: usize = 9;
const POOL_OBSERVATION_KEY_OFFSET: usize = 201;
const POOL_TICK_SPACING_OFFSET: usize = 235;
const POOL_TICK_CURRENT_OFFSET: usize = 269;

/// Bootstrap all CLMM pools in the registry
pub fn bootstrap_clmm_pools(rpc: &RpcClient, registry: &mut PoolRegistry) -> Result<()> {
    let clmm_pools: Vec<Pubkey> = registry.pools
        .iter()
        .filter(|(_, p)| p.dex == DexType::RaydiumClmm)
        .map(|(addr, _)| *addr)
        .collect();
    
    if clmm_pools.is_empty() {
        info!("No Raydium CLMM pools to bootstrap");
        return Ok(());
    }
    
    info!("Bootstrapping {} Raydium CLMM pools...", clmm_pools.len());
    
    for pool_addr in clmm_pools {
        match bootstrap_single_clmm_pool(rpc, registry, &pool_addr) {
            Ok(_) => debug!("Bootstrapped CLMM pool {}", pool_addr),
            Err(e) => warn!("Failed to bootstrap CLMM pool {}: {}", pool_addr, e),
        }
    }
    
    Ok(())
}

/// Bootstrap a single CLMM pool
fn bootstrap_single_clmm_pool(
    rpc: &RpcClient,
    registry: &mut PoolRegistry,
    pool_addr: &Pubkey,
) -> Result<()> {
    // Fetch pool state
    let pool_account = rpc.get_account(pool_addr)?;
    let pool_data = &pool_account.data;
    
    if pool_data.len() < 300 {
        return Err(anyhow::anyhow!("Pool account too small"));
    }
    
    // Parse pool state
    let amm_config = Pubkey::try_from(&pool_data[POOL_AMM_CONFIG_OFFSET..POOL_AMM_CONFIG_OFFSET+32])?;
    let observation_state = Pubkey::try_from(&pool_data[POOL_OBSERVATION_KEY_OFFSET..POOL_OBSERVATION_KEY_OFFSET+32])?;
    let tick_spacing = u16::from_le_bytes([
        pool_data[POOL_TICK_SPACING_OFFSET],
        pool_data[POOL_TICK_SPACING_OFFSET+1]
    ]) as i32;
    let current_tick = i32::from_le_bytes([
        pool_data[POOL_TICK_CURRENT_OFFSET],
        pool_data[POOL_TICK_CURRENT_OFFSET+1],
        pool_data[POOL_TICK_CURRENT_OFFSET+2],
        pool_data[POOL_TICK_CURRENT_OFFSET+3],
    ]);
    
    // Discover tick arrays
    let tick_arrays = discover_tick_arrays(rpc, pool_addr, tick_spacing)?;
    
    // Update pool in registry
    if let Some(pool) = registry.get_pool_mut(pool_addr) {
        pool.amm_config = Some(amm_config);
        pool.observation_state = Some(observation_state);
        pool.tick_spacing = Some(tick_spacing);
        pool.current_tick = Some(current_tick);
        pool.tick_arrays = tick_arrays;
        
        debug!(
            "Pool {} bootstrapped: tick={}, {} tick arrays",
            pool_addr,
            current_tick,
            pool.tick_arrays.len()
        );
    }
    
    Ok(())
}

/// Discover which tick arrays exist for a pool
fn discover_tick_arrays(
    rpc: &RpcClient,
    pool: &Pubkey,
    tick_spacing: i32,
) -> Result<Vec<TickArrayInfo>> {
    let possible_starts = all_tick_array_starts(tick_spacing);
    
    // Derive all possible PDAs
    let pdas: Vec<(Pubkey, i32)> = possible_starts
        .iter()
        .map(|&start_idx| (derive_tick_array_pda(pool, start_idx), start_idx))
        .collect();
    
    let mut existing: Vec<TickArrayInfo> = Vec::new();
    
    // Check in batches of 100 (RPC limit)
    for chunk in pdas.chunks(100) {
        let pubkeys: Vec<Pubkey> = chunk.iter().map(|(pk, _)| *pk).collect();
        let accounts = rpc.get_multiple_accounts(&pubkeys)?;
        
        for (i, maybe_account) in accounts.iter().enumerate() {
            if maybe_account.is_some() {
                existing.push(TickArrayInfo {
                    pubkey: chunk[i].0,
                    start_tick_index: chunk[i].1,
                });
            }
        }
    }
    
    // Sort by start_tick_index
    existing.sort_by_key(|t| t.start_tick_index);
    
    Ok(existing)
}

/// Update current_tick for a pool (called when pool state changes)
pub fn update_pool_tick(pool: &mut Pool, pool_data: &[u8]) {
    if pool_data.len() > POOL_TICK_CURRENT_OFFSET + 4 {
        let current_tick = i32::from_le_bytes([
            pool_data[POOL_TICK_CURRENT_OFFSET],
            pool_data[POOL_TICK_CURRENT_OFFSET+1],
            pool_data[POOL_TICK_CURRENT_OFFSET+2],
            pool_data[POOL_TICK_CURRENT_OFFSET+3],
        ]);
        pool.current_tick = Some(current_tick);
    }
}
