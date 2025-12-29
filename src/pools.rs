//! Static pool loading for zero-RPC runtime
//!
//! Loads pool data from a JSON file at startup, enabling Geyser subscriptions
//! for vault accounts before dynamic discovery begins. This eliminates RPC
//! calls during runtime by pre-populating the PoolStateManager.
//!
//! The pools file contains ONLY static/structural data:
//! - Pool address and DEX type
//! - Token mints and vault addresses (critical for Geyser reserve tracking)
//! - Token decimals
//! - Optional swap instruction accounts (can be derived if omitted)
//!
//! Dynamic state is ALWAYS fetched from Geyser/RPC bootstrap:
//! - sqrt_price, liquidity, tick/bin (CLMM/DLMM state)
//! - fee rates (including dynamic DLMM fees)
//! - spacing/bin_step (immutable but read from chain)

use crate::amm_math::CurveType;
use crate::pool_discovery::{derive_raydium_amm_authority, derive_whirlpool_oracle};
use crate::pool_state::{PoolState, PoolStateManager};
use crate::shredstream::programs::DexType;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use std::path::Path;
use std::str::FromStr;

/// A pool definition loaded from the static pools file.
/// 
/// Contains ONLY static/structural data. All dynamic state (sqrt_price, liquidity,
/// fees, tick/bin) is populated at runtime from Geyser/RPC bootstrap.
/// 
/// Note: Extra fields like "_comment" in JSON are ignored (serde default behavior)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticPool {
    /// Pool address (base58)
    pub address: String,
    /// DEX type: "RaydiumAmm", "RaydiumClmm", "OrcaWhirlpool", "MeteoraDlmm", "MeteoraDamm", "PancakeSwapClmm"
    pub dex: String,
    /// Base/Token A mint address
    pub token_mint_a: String,
    /// Quote/Token B mint address
    pub token_mint_b: String,
    /// Token A vault address (for Geyser subscription)
    pub token_vault_a: String,
    /// Token B vault address (for Geyser subscription)
    pub token_vault_b: String,
    /// Token A decimals
    #[serde(default = "default_decimals")]
    pub decimals_a: u8,
    /// Token B decimals
    #[serde(default = "default_decimals")]
    pub decimals_b: u8,

    // === Optional swap instruction accounts (can be derived if omitted) ===
    /// AMM config account (Raydium CLMM, PancakeSwap CLMM)
    #[serde(default)]
    pub amm_config: Option<String>,
    /// Observation/Oracle state (CLMM pools)
    #[serde(default)]
    pub observation_state: Option<String>,
    /// Bin array bitmap extension (DLMM, optional)
    #[serde(default)]
    pub bin_array_bitmap_extension: Option<String>,
}

fn default_decimals() -> u8 {
    9
}

/// Pools file structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolsFile {
    /// List of pools to track
    pub pools: Vec<StaticPool>,
}

impl PoolsFile {
    /// Load pools from a JSON file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read pools file: {}", path.display()))?;
        
        let pools_file: PoolsFile = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse pools file: {}", path.display()))?;
        
        tracing::info!("Loaded {} pools from {}", pools_file.pools.len(), path.display());
        Ok(pools_file)
    }

    /// Register all pools with the PoolStateManager
    /// Pools loaded from pools.json are marked as vault-subscribed since
    /// they'll be included in the initial Geyser subscription
    pub fn register_with_psm(&self, psm: &PoolStateManager) -> Result<usize> {
        let mut registered = 0;

        for pool in &self.pools {
            match pool.to_pool_state() {
                Ok(state) => {
                    // Use register_pool_with_subscription to mark these as subscribed
                    psm.register_pool_with_subscription(state, true);
                    registered += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse pool {}: {}",
                        pool.address,
                        e
                    );
                }
            }
        }

        // Log vault_index size to verify vaults are being indexed
        let vault_count = psm.all_vaults().len();
        tracing::info!(
            "Registered {}/{} pools with PoolStateManager (vault_index has {} entries, all marked as subscribed)",
            registered,
            self.pools.len(),
            vault_count
        );

        Ok(registered)
    }
}

impl StaticPool {
    /// Parse DEX type from string
    fn parse_dex(&self) -> Result<DexType> {
        match self.dex.as_str() {
            "RaydiumAmm" | "raydium_amm" => Ok(DexType::RaydiumAmm),
            "RaydiumClmm" | "raydium_clmm" => Ok(DexType::RaydiumClmm),
            "OrcaWhirlpool" | "orca_whirlpool" | "Orca" => Ok(DexType::OrcaWhirlpool),
            "MeteoraDlmm" | "meteora_dlmm" | "DLMM" => Ok(DexType::MeteoraDlmm),
            "MeteoraDamm" | "meteora_damm" | "DAMM" => Ok(DexType::MeteoraDamm),
            "PancakeSwapClmm" | "pancakeswap_clmm" | "PancakeSwap" => Ok(DexType::PancakeSwapClmm),
            _ => anyhow::bail!("Unknown DEX type: {}", self.dex),
        }
    }

    /// Determine curve type from DEX type
    fn curve_type(dex: DexType) -> CurveType {
        match dex {
            DexType::RaydiumAmm => CurveType::ConstantProduct,
            // Meteora DAMM v2 uses concentrated liquidity with sqrt_price/liquidity (NOT constant product!)
            // Reference: MeteoraAg/damm-v2 curve.rs - uses √P' = √P * L / (L + Δa * √P)
            DexType::RaydiumClmm | DexType::OrcaWhirlpool | DexType::PancakeSwapClmm | DexType::MeteoraDamm => {
                CurveType::ConcentratedLiquidity
            }
            DexType::MeteoraDlmm => CurveType::Dlmm,
        }
    }

    /// Convert to PoolState for registration
    pub fn to_pool_state(&self) -> Result<PoolState> {
        let address = Pubkey::from_str(&self.address)
            .with_context(|| format!("Invalid pool address: {}", self.address))?;
        let dex = self.parse_dex()?;
        let curve = Self::curve_type(dex);

        let token_mint_a = Pubkey::from_str(&self.token_mint_a)
            .with_context(|| format!("Invalid token_mint_a: {}", self.token_mint_a))?;
        let token_mint_b = Pubkey::from_str(&self.token_mint_b)
            .with_context(|| format!("Invalid token_mint_b: {}", self.token_mint_b))?;
        let token_vault_a = Pubkey::from_str(&self.token_vault_a)
            .with_context(|| format!("Invalid token_vault_a: {}", self.token_vault_a))?;
        let token_vault_b = Pubkey::from_str(&self.token_vault_b)
            .with_context(|| format!("Invalid token_vault_b: {}", self.token_vault_b))?;

        // Parse optional accounts
        let amm_config = self
            .amm_config
            .as_ref()
            .map(|s| Pubkey::from_str(s))
            .transpose()
            .with_context(|| "Invalid amm_config")?;
        
        let observation_state = self
            .observation_state
            .as_ref()
            .map(|s| Pubkey::from_str(s))
            .transpose()
            .with_context(|| "Invalid observation_state")?;
        
        let bin_array_bitmap_extension = self
            .bin_array_bitmap_extension
            .as_ref()
            .map(|s| Pubkey::from_str(s))
            .transpose()
            .with_context(|| "Invalid bin_array_bitmap_extension")?;

        // Create base state - reserves initialized to 0, Geyser/bootstrap will populate
        // All dynamic state (fees, sqrt_price, liquidity, tick/bin) comes from Geyser
        let mut state = PoolState::new(
            address,
            dex,
            token_mint_a,
            token_mint_b,
            0, // base_reserve - Geyser will update
            0, // quote_reserve - Geyser will update
            self.decimals_a,
            self.decimals_b,
            curve,
            0, // slot will be updated by Geyser
        );

        // Derive PDAs based on DEX type
        // Note: tick/bin arrays are derived at runtime from Geyser state, not from pools.json
        let (amm_authority, derived_observation, pool_authority) =
            Self::derive_pdas(&address, dex, observation_state)?;

        // Set metadata (tick/bin arrays will be populated by bootstrap/Geyser)
        state.set_metadata(
            token_vault_a,
            token_vault_b,
            amm_authority,
            amm_config,
            derived_observation,
            vec![], // tick_arrays - populated by bootstrap
            bin_array_bitmap_extension,
            vec![], // bin_arrays - populated by bootstrap
            pool_authority,
        );

        Ok(state)
    }

    /// Derive PDAs for swap instruction building
    /// Note: tick/bin arrays are derived at runtime when we have current tick/bin from Geyser
    fn derive_pdas(
        pool: &Pubkey,
        dex: DexType,
        observation_override: Option<Pubkey>,
    ) -> Result<(
        Option<Pubkey>,           // amm_authority
        Option<Pubkey>,           // observation_state
        Option<Pubkey>,           // pool_authority
    )> {
        match dex {
            DexType::RaydiumAmm => {
                let authority = derive_raydium_amm_authority();
                Ok((Some(authority), None, None))
            }
            DexType::RaydiumClmm | DexType::PancakeSwapClmm => {
                // Tick arrays derived at runtime from Geyser state
                Ok((None, observation_override, None))
            }
            DexType::OrcaWhirlpool => {
                let oracle = observation_override.unwrap_or_else(|| derive_whirlpool_oracle(pool));
                // Tick arrays derived at runtime from Geyser state
                Ok((None, Some(oracle), None))
            }
            DexType::MeteoraDlmm => {
                // Bin arrays derived at runtime from Geyser state
                Ok((None, None, None))
            }
            DexType::MeteoraDamm => {
                // DAMM v2 uses a GLOBAL pool authority (unlike DLMM which is per-pool)
                // The seed is just "pool_authority" with NO pool address suffix
                // See: https://github.com/MeteoraAg/damm-v2/blob/main/programs/cp-amm/src/const_pda.rs
                let pool_authority = Pubkey::find_program_address(
                    &[b"pool_authority"],
                    &crate::shredstream::programs::METEORA_DAMM,
                )
                .0;
                Ok((None, None, Some(pool_authority)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dex_types() {
        let pool = StaticPool {
            address: "11111111111111111111111111111111".to_string(),
            dex: "RaydiumAmm".to_string(),
            token_mint_a: "So11111111111111111111111111111111111111112".to_string(),
            token_mint_b: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_string(),
            token_vault_a: "11111111111111111111111111111111".to_string(),
            token_vault_b: "11111111111111111111111111111111".to_string(),
            decimals_a: 9,
            decimals_b: 6,
            amm_config: None,
            observation_state: None,
            bin_array_bitmap_extension: None,
        };

        assert_eq!(pool.parse_dex().unwrap(), DexType::RaydiumAmm);
    }

    #[test]
    fn test_pools_file_deserialize() {
        // Test that pools.json format without stale fields works
        let json = r#"{
            "pools": [
                {
                    "address": "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2",
                    "dex": "RaydiumAmm",
                    "token_mint_a": "So11111111111111111111111111111111111111112",
                    "token_mint_b": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                    "token_vault_a": "DQyrAcCrDXQ7NeoqGgDCZwBvWDcYmFCjSb9JtteuvPpz",
                    "token_vault_b": "HLmqeL62xR1QoZ1HKKbXRrdN1p3phKpxRMb2VVopvBBz",
                    "decimals_a": 9,
                    "decimals_b": 6
                }
            ]
        }"#;

        let pools_file: PoolsFile = serde_json::from_str(json).unwrap();
        assert_eq!(pools_file.pools.len(), 1);
        assert_eq!(pools_file.pools[0].dex, "RaydiumAmm");
    }

    #[test]
    fn test_pools_file_deserialize_with_legacy_fields() {
        // Test backward compatibility: old pools.json with stale fields should still parse
        // (serde ignores unknown fields by default)
        let json = r#"{
            "pools": [
                {
                    "address": "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2",
                    "dex": "RaydiumAmm",
                    "token_mint_a": "So11111111111111111111111111111111111111112",
                    "token_mint_b": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                    "token_vault_a": "DQyrAcCrDXQ7NeoqGgDCZwBvWDcYmFCjSb9JtteuvPpz",
                    "token_vault_b": "HLmqeL62xR1QoZ1HKKbXRrdN1p3phKpxRMb2VVopvBBz",
                    "decimals_a": 9,
                    "decimals_b": 6,
                    "fee_bps": 25,
                    "spacing": 1,
                    "current_tick_or_bin": 0,
                    "sqrt_price": "18446744073709551616",
                    "liquidity": "1000000"
                }
            ]
        }"#;

        let pools_file: PoolsFile = serde_json::from_str(json).unwrap();
        assert_eq!(pools_file.pools.len(), 1);
        assert_eq!(pools_file.pools[0].dex, "RaydiumAmm");
    }
}
