//! Flat configuration - no nesting, no feature flags
//!
//! Load from JSON, validate, done.

use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Helius RPC endpoint (with API key)
    pub helius_rpc: String,
    /// Helius WebSocket endpoint (with API key)
    pub helius_ws: String,
    /// Helius Sender endpoint for fast tx submission
    #[serde(default = "default_helius_sender")]
    pub helius_sender: String,
    /// Jito block engine endpoint (legacy, optional)
    #[serde(default)]
    pub jito_endpoint: String,
    /// Path to Jito auth keypair (legacy, optional)
    #[serde(default)]
    pub jito_auth_keypair: String,
    /// Path to payer/signer keypair
    pub payer_keypair: String,
    /// Path to pools JSON file
    pub pools_file: String,
    /// Minimum profit in lamports to execute arb
    #[serde(default = "default_min_profit")]
    pub min_profit_lamports: u64,
    /// Slippage tolerance in basis points
    #[serde(default = "default_slippage_bps")]
    pub slippage_bps: u16,
    /// Tip percentage of profit in basis points (e.g., 7500 = 75%)
    #[serde(default = "default_tip_bps")]
    pub tip_bps: u16,
    /// Compute units for arb transaction
    #[serde(default = "default_compute_units")]
    pub compute_units: u32,
    /// Compute unit price in micro-lamports
    #[serde(default = "default_compute_unit_price")]
    pub compute_unit_price: u64,
    /// Flash loan config (Solend SOL reserve)
    #[serde(default)]
    pub flash_loan: FlashLoanConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FlashLoanConfig {
    pub program_id: String,
    pub lending_market: String,
    pub reserve: String,
    pub reserve_liquidity_supply: String,
    pub reserve_liquidity_fee_receiver: String,
    pub liquidity_mint: String,
}

impl Default for FlashLoanConfig {
    fn default() -> Self {
        // Solend mainnet SOL reserve
        Self {
            program_id: "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo".to_string(),
            lending_market: "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY".to_string(),
            reserve: "8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36".to_string(),
            reserve_liquidity_supply: "8UviNr47S8eL6J3WfDxMRa3hvLta1VDJwNWqsDgtN3Cv".to_string(),
            reserve_liquidity_fee_receiver: "5wo1tFpi4HaVKnemqaXeQnBEpezrJXcXvuztYaPhvgC7".to_string(),
            liquidity_mint: "So11111111111111111111111111111111111111112".to_string(),
        }
    }
}

impl FlashLoanConfig {
    pub fn program_id(&self) -> Pubkey {
        Pubkey::from_str(&self.program_id).expect("invalid flash loan program_id")
    }
    pub fn lending_market(&self) -> Pubkey {
        Pubkey::from_str(&self.lending_market).expect("invalid lending_market")
    }
    pub fn reserve(&self) -> Pubkey {
        Pubkey::from_str(&self.reserve).expect("invalid reserve")
    }
    pub fn reserve_liquidity_supply(&self) -> Pubkey {
        Pubkey::from_str(&self.reserve_liquidity_supply).expect("invalid reserve_liquidity_supply")
    }
    pub fn reserve_liquidity_fee_receiver(&self) -> Pubkey {
        Pubkey::from_str(&self.reserve_liquidity_fee_receiver).expect("invalid fee_receiver")
    }
    pub fn liquidity_mint(&self) -> Pubkey {
        Pubkey::from_str(&self.liquidity_mint).expect("invalid liquidity_mint")
    }
}

fn default_min_profit() -> u64 { 100_000 } // 0.0001 SOL
fn default_slippage_bps() -> u16 { 150 }   // 1.5%
fn default_tip_bps() -> u16 { 7500 }       // 75% of profit
fn default_compute_units() -> u32 { 400_000 }
fn default_compute_unit_price() -> u64 { 1000 }
fn default_helius_sender() -> String { "https://sender.helius-rpc.com/fast".to_string() }

impl Config {
    /// Load config from JSON file
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate config values
    fn validate(&self) -> anyhow::Result<()> {
        if self.helius_rpc.is_empty() {
            anyhow::bail!("helius_rpc is required");
        }
        if self.helius_ws.is_empty() {
            anyhow::bail!("helius_ws is required");
        }
        if self.helius_sender.is_empty() {
            anyhow::bail!("helius_sender is required");
        }
        if !Path::new(&self.payer_keypair).exists() {
            anyhow::bail!("payer_keypair file not found: {}", self.payer_keypair);
        }
        if !Path::new(&self.pools_file).exists() {
            anyhow::bail!("pools_file not found: {}", self.pools_file);
        }
        Ok(())
    }

    /// Load keypair from file path
    pub fn load_keypair(path: &str) -> anyhow::Result<Keypair> {
        let bytes = std::fs::read_to_string(path)?;
        let key_bytes: Vec<u8> = serde_json::from_str(&bytes)?;
        Ok(Keypair::from_bytes(&key_bytes)?)
    }

    pub fn jito_auth(&self) -> anyhow::Result<Keypair> {
        Self::load_keypair(&self.jito_auth_keypair)
    }

    pub fn payer(&self) -> anyhow::Result<Keypair> {
        Self::load_keypair(&self.payer_keypair)
    }
}
