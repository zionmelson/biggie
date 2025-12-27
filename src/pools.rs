//! Static pool loading for zero-RPC runtime
//!
//! Loads pool data from a JSON file at startup, enabling Geyser subscriptions
//! for vault accounts before dynamic discovery begins. This eliminates RPC
//! calls during runtime by pre-populating the PoolStateManager.
//!
//! The pools file contains:
//! - Pool address and DEX type
//! - Token mints and vault addresses (critical for Geyser reserve tracking)
//! - CLMM/DLMM parameters (spacing, fee)
//! - Swap instruction accounts (derived PDAs can be omitted)

use crate::amm_math::CurveType;
use crate::pool_discovery::{
    derive_dlmm_bin_arrays, derive_raydium_amm_authority, derive_raydium_clmm_tick_arrays,
    derive_whirlpool_oracle, derive_whirlpool_tick_arrays,
};
use crate::pool_state::{PoolState, PoolStateManager};
use crate::shredstream::programs::DexType;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use std::path::Path;
use std::str::FromStr;

/// A pool definition loaded from the static pools file
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
    /// Fee in basis points
    #[serde(default = "default_fee_bps")]
    pub fee_bps: u16,
    /// Tick spacing (CLMM) or bin step (DLMM), 0 for AMM
    #[serde(default)]
    pub spacing: u16,
    /// Current tick/bin (optional, will be updated via Geyser)
    #[serde(default)]
    pub current_tick_or_bin: i32,
    /// Initial base reserve (optional, will be updated via Geyser)
    #[serde(default)]
    pub base_reserve: u64,
    /// Initial quote reserve (optional, will be updated via Geyser)
    #[serde(default)]
    pub quote_reserve: u64,

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

fn default_fee_bps() -> u16 {
    25
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
    pub fn register_with_psm(&self, psm: &PoolStateManager) -> Result<usize> {
        let mut registered = 0;

        for pool in &self.pools {
            match pool.to_pool_state() {
                Ok(state) => {
                    psm.register_pool(state);
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
            "Registered {}/{} pools with PoolStateManager (vault_index has {} entries)",
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
            DexType::RaydiumAmm | DexType::MeteoraDamm => CurveType::ConstantProduct,
            DexType::RaydiumClmm | DexType::OrcaWhirlpool | DexType::PancakeSwapClmm => {
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

        // Create base state
        let mut state = PoolState::new(
            address,
            dex,
            token_mint_a,
            token_mint_b,
            self.base_reserve,
            self.quote_reserve,
            self.decimals_a,
            self.decimals_b,
            self.fee_bps,
            curve,
            0, // slot will be updated by Geyser
        );

        // Derive PDAs based on DEX type
        let (amm_authority, derived_observation, tick_arrays, bin_arrays, pool_authority) =
            self.derive_pdas(&address, dex, observation_state)?;

        // Set metadata
        state.set_metadata(
            token_vault_a,
            token_vault_b,
            amm_authority,
            amm_config,
            derived_observation,
            tick_arrays,
            bin_array_bitmap_extension,
            bin_arrays,
            pool_authority,
        );

        // Set CLMM/DLMM specific state
        if self.spacing > 0 {
            state.spacing = Some(self.spacing);
            state.confirmed_tick_or_bin = Some(self.current_tick_or_bin);
        }

        Ok(state)
    }

    /// Derive PDAs for swap instruction building
    fn derive_pdas(
        &self,
        pool: &Pubkey,
        dex: DexType,
        observation_override: Option<Pubkey>,
    ) -> Result<(
        Option<Pubkey>,           // amm_authority
        Option<Pubkey>,           // observation_state
        Vec<Pubkey>,              // tick_arrays
        Vec<Pubkey>,              // bin_arrays
        Option<Pubkey>,           // pool_authority
    )> {
        match dex {
            DexType::RaydiumAmm => {
                let authority = derive_raydium_amm_authority();
                Ok((Some(authority), None, vec![], vec![], None))
            }
            DexType::RaydiumClmm | DexType::PancakeSwapClmm => {
                // Tick arrays are derived from current tick
                let tick_arrays = if self.spacing > 0 {
                    derive_raydium_clmm_tick_arrays(pool, self.current_tick_or_bin, self.spacing)
                } else {
                    vec![]
                };
                Ok((None, observation_override, tick_arrays, vec![], None))
            }
            DexType::OrcaWhirlpool => {
                let oracle = observation_override.unwrap_or_else(|| derive_whirlpool_oracle(pool));
                let tick_arrays = if self.spacing > 0 {
                    derive_whirlpool_tick_arrays(pool, self.current_tick_or_bin, self.spacing)
                } else {
                    vec![]
                };
                Ok((None, Some(oracle), tick_arrays, vec![], None))
            }
            DexType::MeteoraDlmm => {
                let bin_arrays = derive_dlmm_bin_arrays(pool, self.current_tick_or_bin);
                Ok((None, None, vec![], bin_arrays, None))
            }
            DexType::MeteoraDamm => {
                // DAMM pool authority is derived
                let pool_authority = Pubkey::find_program_address(
                    &[b"pool_authority", pool.as_ref()],
                    &crate::shredstream::programs::METEORA_DAMM,
                )
                .0;
                Ok((None, None, vec![], vec![], Some(pool_authority)))
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
            fee_bps: 25,
            spacing: 0,
            current_tick_or_bin: 0,
            base_reserve: 0,
            quote_reserve: 0,
            amm_config: None,
            observation_state: None,
            bin_array_bitmap_extension: None,
        };

        assert_eq!(pool.parse_dex().unwrap(), DexType::RaydiumAmm);
    }

    #[test]
    fn test_pools_file_deserialize() {
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
                    "fee_bps": 25
                }
            ]
        }"#;

        let pools_file: PoolsFile = serde_json::from_str(json).unwrap();
        assert_eq!(pools_file.pools.len(), 1);
        assert_eq!(pools_file.pools[0].dex, "RaydiumAmm");
    }
}
