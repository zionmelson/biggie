//! Bootstrap - Initialize CLMM pools with tick array discovery
//!
//! Fetches on-chain data to populate amm_config, observation_state, current_tick,
//! and discovers which tick arrays exist for each CLMM pool.

use crate::pools::{DexType, Pool, PoolRegistry, TickArrayInfo, derive_tick_array_pda, all_tick_array_starts};
use anyhow::Result;
use solana_account_decoder::UiAccountEncoding;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcAccountInfoConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use tracing::{info, debug, warn};

/// Pool state layout offsets for Raydium CLMM (same for PancakeSwap)
const POOL_AMM_CONFIG_OFFSET: usize = 9;
const POOL_OBSERVATION_KEY_OFFSET: usize = 201;
const POOL_TICK_SPACING_OFFSET: usize = 235;
const POOL_LIQUIDITY_OFFSET: usize = 237;      // u128 (16 bytes)
const POOL_SQRT_PRICE_OFFSET: usize = 253;     // u128 (16 bytes)
const POOL_TICK_CURRENT_OFFSET: usize = 269;

/// PancakeSwap CLMM program ID
fn pancakeswap_clmm_program() -> Pubkey {
    Pubkey::from_str("HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq").unwrap()
}

/// Derive PancakeSwap tick array PDA (uses big-endian bytes for tick index)
fn derive_pancakeswap_tick_array_pda(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[b"tick_array", pool.as_ref(), &start_tick_index.to_be_bytes()],
        &pancakeswap_clmm_program(),
    ).0
}

/// Bootstrap all CLMM pools in the registry
pub fn bootstrap_clmm_pools(rpc: &RpcClient, registry: &mut PoolRegistry) -> Result<()> {
    // Bootstrap Raydium CLMM pools
    let raydium_clmm_pools: Vec<Pubkey> = registry.pools
        .iter()
        .filter(|(_, p)| p.dex == DexType::RaydiumClmm)
        .map(|(addr, _)| *addr)
        .collect();
    
    if !raydium_clmm_pools.is_empty() {
        info!("Bootstrapping {} Raydium CLMM pools...", raydium_clmm_pools.len());
        for pool_addr in raydium_clmm_pools {
            match bootstrap_single_clmm_pool(rpc, registry, &pool_addr, false) {
                Ok(_) => debug!("Bootstrapped Raydium CLMM pool {}", pool_addr),
                Err(e) => warn!("Failed to bootstrap Raydium CLMM pool {}: {}", pool_addr, e),
            }
        }
    }
    
    // Bootstrap PancakeSwap CLMM pools  
    let pancake_clmm_pools: Vec<Pubkey> = registry.pools
        .iter()
        .filter(|(_, p)| p.dex == DexType::PancakeSwapClmm)
        .map(|(addr, _)| *addr)
        .collect();
    
    if !pancake_clmm_pools.is_empty() {
        info!("Bootstrapping {} PancakeSwap CLMM pools...", pancake_clmm_pools.len());
        for pool_addr in pancake_clmm_pools {
            match bootstrap_single_clmm_pool(rpc, registry, &pool_addr, true) {
                Ok(_) => debug!("Bootstrapped PancakeSwap CLMM pool {}", pool_addr),
                Err(e) => warn!("Failed to bootstrap PancakeSwap CLMM pool {}: {}", pool_addr, e),
            }
        }
    }
    
    // Bootstrap Meteora DLMM pools
    let dlmm_pools: Vec<Pubkey> = registry.pools
        .iter()
        .filter(|(_, p)| p.dex == DexType::MeteoraDlmm)
        .map(|(addr, _)| *addr)
        .collect();
    
    if !dlmm_pools.is_empty() {
        info!("Bootstrapping {} Meteora DLMM pools...", dlmm_pools.len());
        for pool_addr in dlmm_pools {
            match bootstrap_single_dlmm_pool(rpc, registry, &pool_addr) {
                Ok(_) => debug!("Bootstrapped Meteora DLMM pool {}", pool_addr),
                Err(e) => warn!("Failed to bootstrap Meteora DLMM pool {}: {}", pool_addr, e),
            }
        }
    }
    
    // Bootstrap Orca Whirlpool pools
    let orca_pools: Vec<Pubkey> = registry.pools
        .iter()
        .filter(|(_, p)| p.dex == DexType::OrcaWhirlpool)
        .map(|(addr, _)| *addr)
        .collect();
    
    if !orca_pools.is_empty() {
        info!("Bootstrapping {} Orca Whirlpool pools...", orca_pools.len());
        for pool_addr in orca_pools {
            match bootstrap_single_orca_pool(rpc, registry, &pool_addr) {
                Ok(_) => debug!("Bootstrapped Orca Whirlpool pool {}", pool_addr),
                Err(e) => warn!("Failed to bootstrap Orca Whirlpool pool {}: {}", pool_addr, e),
            }
        }
    }
    
    Ok(())
}

/// Bootstrap a single CLMM pool (works for both Raydium and PancakeSwap)
fn bootstrap_single_clmm_pool(
    rpc: &RpcClient,
    registry: &mut PoolRegistry,
    pool_addr: &Pubkey,
    is_pancakeswap: bool,
) -> Result<()> {
    // Fetch pool state
    let pool_account = rpc.get_account(pool_addr)?;
    let pool_data = &pool_account.data;
    
    if pool_data.len() < 300 {
        return Err(anyhow::anyhow!("Pool account too small"));
    }
    
    // Parse pool state (layout is the same for Raydium and PancakeSwap)
    let amm_config = Pubkey::try_from(&pool_data[POOL_AMM_CONFIG_OFFSET..POOL_AMM_CONFIG_OFFSET+32])?;
    let observation_state = Pubkey::try_from(&pool_data[POOL_OBSERVATION_KEY_OFFSET..POOL_OBSERVATION_KEY_OFFSET+32])?;
    let tick_spacing = u16::from_le_bytes([
        pool_data[POOL_TICK_SPACING_OFFSET],
        pool_data[POOL_TICK_SPACING_OFFSET+1]
    ]) as i32;
    
    // Parse liquidity (u128, 16 bytes)
    let liquidity = u128::from_le_bytes(
        pool_data[POOL_LIQUIDITY_OFFSET..POOL_LIQUIDITY_OFFSET+16]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse liquidity"))?
    );
    
    // Parse sqrt_price_x64 (u128, 16 bytes)
    let sqrt_price_x64 = u128::from_le_bytes(
        pool_data[POOL_SQRT_PRICE_OFFSET..POOL_SQRT_PRICE_OFFSET+16]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse sqrt_price"))?
    );
    
    let current_tick = i32::from_le_bytes([
        pool_data[POOL_TICK_CURRENT_OFFSET],
        pool_data[POOL_TICK_CURRENT_OFFSET+1],
        pool_data[POOL_TICK_CURRENT_OFFSET+2],
        pool_data[POOL_TICK_CURRENT_OFFSET+3],
    ]);
    
    // Discover tick arrays (PDA derivation differs between Raydium and PancakeSwap)
    let tick_arrays = if is_pancakeswap {
        discover_pancakeswap_tick_arrays(rpc, pool_addr, tick_spacing)?
    } else {
        discover_tick_arrays(rpc, pool_addr, tick_spacing)?
    };
    
    // For PancakeSwap, also discover the bitmap extension
    let bitmap_extension = if is_pancakeswap {
        discover_pancakeswap_bitmap_extension(rpc, pool_addr)?
    } else {
        None
    };
    
    // Update pool in registry
    if let Some(pool) = registry.get_pool_mut(pool_addr) {
        pool.amm_config = Some(amm_config);
        pool.observation_state = Some(observation_state);
        pool.tick_spacing = Some(tick_spacing);
        pool.current_tick = Some(current_tick);
        pool.sqrt_price_x64 = Some(sqrt_price_x64);
        pool.liquidity = Some(liquidity);
        pool.tick_arrays = tick_arrays;
        pool.bitmap_extension = bitmap_extension;
        
        debug!(
            "Pool {} bootstrapped: tick={}, sqrt_price={}, liquidity={}, {} tick arrays{}",
            pool_addr,
            current_tick,
            sqrt_price_x64,
            liquidity,
            pool.tick_arrays.len(),
            if bitmap_extension.is_some() { ", with bitmap extension" } else { "" }
        );
    }
    
    Ok(())
}

/// Bootstrap a single Meteora DLMM pool
fn bootstrap_single_dlmm_pool(
    rpc: &RpcClient,
    registry: &mut PoolRegistry,
    pool_addr: &Pubkey,
) -> Result<()> {
    // DLMM layout offsets (8-byte Anchor discriminator + fields)
    const ACTIVE_ID_OFFSET: usize = 76;   // 8 + 68
    const BIN_STEP_OFFSET: usize = 80;    // 8 + 72
    
    // Fetch pool state
    let pool_account = rpc.get_account(pool_addr)?;
    let pool_data = &pool_account.data;
    
    if pool_data.len() < BIN_STEP_OFFSET + 2 {
        return Err(anyhow::anyhow!("DLMM pool account too small"));
    }
    
    // Parse active_bin_id (i32)
    let active_bin_id = i32::from_le_bytes([
        pool_data[ACTIVE_ID_OFFSET],
        pool_data[ACTIVE_ID_OFFSET + 1],
        pool_data[ACTIVE_ID_OFFSET + 2],
        pool_data[ACTIVE_ID_OFFSET + 3],
    ]);
    
    // Parse bin_step (u16)
    let bin_step = u16::from_le_bytes([
        pool_data[BIN_STEP_OFFSET],
        pool_data[BIN_STEP_OFFSET + 1],
    ]);
    
    // Update pool in registry
    if let Some(pool) = registry.get_pool_mut(pool_addr) {
        pool.active_bin_id = Some(active_bin_id);
        pool.bin_step = Some(bin_step);
        
        debug!(
            "DLMM pool {} bootstrapped: active_bin={}, bin_step={}",
            pool_addr,
            active_bin_id,
            bin_step,
        );
    }
    
    Ok(())
}

/// Bootstrap a single Orca Whirlpool pool
fn bootstrap_single_orca_pool(
    rpc: &RpcClient,
    registry: &mut PoolRegistry,
    pool_addr: &Pubkey,
) -> Result<()> {
    // Orca Whirlpool layout offsets (includes 8-byte Anchor discriminator)
    const SQRT_PRICE_OFFSET: usize = 65;
    const TICK_OFFSET: usize = 261;
    const LIQUIDITY_OFFSET: usize = 265;
    const TICK_SPACING_OFFSET: usize = 281;
    
    // Fetch pool state
    let pool_account = rpc.get_account(pool_addr)?;
    let pool_data = &pool_account.data;
    
    if pool_data.len() < TICK_SPACING_OFFSET + 2 {
        return Err(anyhow::anyhow!("Orca Whirlpool account too small"));
    }
    
    // Parse sqrt_price (u128)
    let sqrt_price_x64 = u128::from_le_bytes(
        pool_data[SQRT_PRICE_OFFSET..SQRT_PRICE_OFFSET+16]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse sqrt_price"))?
    );
    
    // Parse tick_current_index (i32)
    let current_tick = i32::from_le_bytes([
        pool_data[TICK_OFFSET],
        pool_data[TICK_OFFSET + 1],
        pool_data[TICK_OFFSET + 2],
        pool_data[TICK_OFFSET + 3],
    ]);
    
    // Parse liquidity (u128)
    let liquidity = u128::from_le_bytes(
        pool_data[LIQUIDITY_OFFSET..LIQUIDITY_OFFSET+16]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse liquidity"))?
    );
    
    // Parse tick_spacing (u16)
    let tick_spacing = u16::from_le_bytes([
        pool_data[TICK_SPACING_OFFSET],
        pool_data[TICK_SPACING_OFFSET + 1],
    ]) as i32;
    
    // Update pool in registry
    if let Some(pool) = registry.get_pool_mut(pool_addr) {
        pool.sqrt_price_x64 = Some(sqrt_price_x64);
        pool.current_tick = Some(current_tick);
        pool.liquidity = Some(liquidity);
        pool.tick_spacing = Some(tick_spacing);
        
        debug!(
            "Orca Whirlpool {} bootstrapped: tick={}, sqrt_price={}, liquidity={}",
            pool_addr,
            current_tick,
            sqrt_price_x64,
            liquidity,
        );
    }
    
    Ok(())
}

/// Discover PancakeSwap bitmap extension account
fn discover_pancakeswap_bitmap_extension(
    rpc: &RpcClient,
    pool: &Pubkey,
) -> Result<Option<Pubkey>> {
    // PancakeSwap TickArrayBitmapExtension discriminator
    const BITMAP_EXT_DISCRIMINATOR: [u8; 8] = [0x3c, 0x96, 0x24, 0xdb, 0x61, 0x80, 0x8b, 0x99];
    
    // Query getProgramAccounts with filters for this pool
    use solana_client::rpc_config::RpcProgramAccountsConfig;
    use solana_client::rpc_filter::{RpcFilterType, Memcmp};
    
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(0, BITMAP_EXT_DISCRIMINATOR.to_vec())),
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(8, pool.to_bytes().to_vec())),
        ]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            commitment: Some(CommitmentConfig::confirmed()),
            data_slice: None,
            min_context_slot: None,
        },
        with_context: None,
        sort_results: None,
    };
    
    let accounts = rpc.get_program_accounts_with_config(&pancakeswap_clmm_program(), config)?;
    
    if accounts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(accounts[0].0))
    }
}

/// Discover which tick arrays exist for a Raydium CLMM pool
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
    // Use base64 encoding to avoid "Encoded binary (base 58) data should be less than 128 bytes" error
    let config = RpcAccountInfoConfig {
        encoding: Some(UiAccountEncoding::Base64),
        commitment: Some(CommitmentConfig::confirmed()),
        data_slice: None,
        min_context_slot: None,
    };
    
    for chunk in pdas.chunks(100) {
        let pubkeys: Vec<Pubkey> = chunk.iter().map(|(pk, _)| *pk).collect();
        let accounts = rpc.get_multiple_accounts_with_config(&pubkeys, config.clone())?.value;
        
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

/// Discover which tick arrays exist for a PancakeSwap CLMM pool
/// PancakeSwap uses big-endian bytes for tick array PDAs
fn discover_pancakeswap_tick_arrays(
    rpc: &RpcClient,
    pool: &Pubkey,
    tick_spacing: i32,
) -> Result<Vec<TickArrayInfo>> {
    let possible_starts = all_tick_array_starts(tick_spacing);
    
    // Derive all possible PDAs using PancakeSwap's big-endian format
    let pdas: Vec<(Pubkey, i32)> = possible_starts
        .iter()
        .map(|&start_idx| (derive_pancakeswap_tick_array_pda(pool, start_idx), start_idx))
        .collect();
    
    let mut existing: Vec<TickArrayInfo> = Vec::new();
    
    // Check in batches of 100 (RPC limit)
    // Use base64 encoding to avoid "Encoded binary (base 58) data should be less than 128 bytes" error
    let config = RpcAccountInfoConfig {
        encoding: Some(UiAccountEncoding::Base64),
        commitment: Some(CommitmentConfig::confirmed()),
        data_slice: None,
        min_context_slot: None,
    };
    
    for chunk in pdas.chunks(100) {
        let pubkeys: Vec<Pubkey> = chunk.iter().map(|(pk, _)| *pk).collect();
        let accounts = rpc.get_multiple_accounts_with_config(&pubkeys, config.clone())?.value;
        
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
