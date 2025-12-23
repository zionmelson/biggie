use serde::{Deserialize, Serialize};
use solana_sdk::signature::Keypair;
use solana_sdk::signer::keypair::read_keypair_file;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub anchor_tokens: Vec<AnchorToken>,
    #[serde(default)]
    pub solend_flash_loans: HashMap<String, SolendFlashLoan>,
    #[serde(default)]
    pub jito: JitoConfig,
    #[serde(default)]
    pub wallets: WalletsConfig,
    #[serde(default)]
    pub quicknode_rpcs: Vec<String>,
    #[serde(default)]
    pub lookup_tables: Vec<LookupTableConfig>,
    #[serde(default)]
    pub shredstream: ShredstreamConfig,
    /// Geyser plugin configuration - primary data source for local validator
    #[serde(default)]
    pub geyser: GeyserPluginConfig,
    /// Local RPC endpoint - use this instead of quicknode when running on validator
    #[serde(default)]
    pub local_rpc: LocalRpcConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LookupTableConfig {
    pub label: String,
    pub address: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SolendFlashLoan {
    pub program_id: String,
    pub lending_market: String,
    pub reserve: String,
    pub reserve_liquidity_mint: String,
    pub borrow: FlashLoanBorrow,
    pub repay: FlashLoanRepay,
    /// Maximum borrow amount in base units (optional, defaults to 1B)
    #[serde(default = "default_max_borrow")]
    pub max_borrow: u64,
    /// Minimum borrow amount in base units (optional, defaults to 100M)  
    #[serde(default = "default_min_borrow")]
    pub min_borrow: u64,
}

fn default_max_borrow() -> u64 {
    1_000_000_000 // 1B base units - reasonable default for most tokens
}

fn default_min_borrow() -> u64 {
    100_000_000 // 100M base units - 0.1 SOL or $100 USDC
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlashLoanBorrow {
    pub reserve_liquidity_supply: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlashLoanRepay {
    pub reserve_liquidity_supply: String,
    pub reserve_liquidity_fee_receiver: String,
    pub host_fee_receiver: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnchorToken {
    pub mint: String,
    pub flash_loan_provider: String, // e.g., "solend"
    pub max_borrow: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct JitoConfig {
    #[serde(default)]
    pub endpoints: Vec<String>,
    pub ca_pem: Option<String>,
    pub tip_floor_url: Option<String>,
    pub tip_stream_url: Option<String>,
}

impl JitoConfig {
    /// Returns the primary endpoint or a default.
    pub fn primary_endpoint(&self) -> String {
        self.endpoints
            .first()
            .cloned()
            .unwrap_or_else(|| "https://ny.mainnet.block-engine.jito.wtf".to_string())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShredstreamConfig {
    /// Whether shredstream is enabled
    #[serde(default)]
    pub enabled: bool,
    /// gRPC endpoint URL for the local shredstream proxy (e.g., "http://127.0.0.1:10000")
    #[serde(default = "default_shredstream_endpoint")]
    pub endpoint: String,
    /// MEV strategy settings
    #[serde(default)]
    pub mev: MevConfig,
}

/// MEV strategy configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MevConfig {
    /// Enable backrunning (follow victim swaps)
    #[serde(default = "default_true")]
    pub backrun_enabled: bool,
    /// Enable sandwich attacks (front + back)
    #[serde(default)]
    pub sandwich_enabled: bool,
    /// Minimum swap amount in lamports to consider for MEV
    #[serde(default = "default_min_swap_amount")]
    pub min_swap_amount: u64,
    /// Minimum expected profit in lamports to submit a bundle
    #[serde(default = "default_min_profit")]
    pub min_profit_lamports: u64,
    /// Minimum victim slippage in bps to consider for MEV
    #[serde(default = "default_min_slippage")]
    pub min_victim_slippage_bps: u64,
    /// Compute units to request per transaction
    #[serde(default = "default_compute_units")]
    pub compute_units: u32,
    /// Compute unit price in micro-lamports
    #[serde(default = "default_compute_price")]
    pub compute_unit_price: u64,
    /// Tip as percentage of profit (in bps, e.g., 5000 = 50%)
    #[serde(default = "default_tip_share")]
    pub tip_profit_share_bps: u64,
}

fn default_true() -> bool { true }
fn default_min_swap_amount() -> u64 { 10_000_000 } // 0.01 SOL
fn default_min_profit() -> u64 { 50_000 } // 0.00005 SOL
fn default_min_slippage() -> u64 { 30 } // 0.3%
fn default_compute_units() -> u32 { 400_000 }
fn default_compute_price() -> u64 { 1000 }
fn default_tip_share() -> u64 { 5000 } // 50%

impl Default for MevConfig {
    fn default() -> Self {
        Self {
            backrun_enabled: true,
            sandwich_enabled: false,
            min_swap_amount: default_min_swap_amount(),
            min_profit_lamports: default_min_profit(),
            min_victim_slippage_bps: default_min_slippage(),
            compute_units: default_compute_units(),
            compute_unit_price: default_compute_price(),
            tip_profit_share_bps: default_tip_share(),
        }
    }
}

fn default_shredstream_endpoint() -> String {
    "http://127.0.0.1:10000".to_string()
}

impl Default for ShredstreamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_shredstream_endpoint(),
            mev: MevConfig::default(),
        }
    }
}

/// Geyser plugin configuration for direct validator access
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeyserPluginConfig {
    /// Whether to use Geyser instead of shredstream proxy
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// gRPC endpoint URL for the Geyser plugin (default: localhost:10000)
    #[serde(default = "default_geyser_endpoint")]
    pub endpoint: String,
    /// Connection timeout in milliseconds
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// Request timeout in milliseconds
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    /// Skip vote transactions (recommended: true, votes are ~90% of traffic)
    #[serde(default = "default_true")]
    pub skip_votes: bool,
}

fn default_geyser_endpoint() -> String {
    "http://localhost:10000".to_string()
}

fn default_connect_timeout_ms() -> u64 {
    10_000 // 10 seconds
}

fn default_request_timeout_ms() -> u64 {
    30_000 // 30 seconds
}

impl Default for GeyserPluginConfig {
    fn default() -> Self {
        Self {
            enabled: true, // Geyser is the preferred mode
            endpoint: default_geyser_endpoint(),
            connect_timeout_ms: default_connect_timeout_ms(),
            request_timeout_ms: default_request_timeout_ms(),
            skip_votes: true,
        }
    }
}

impl GeyserPluginConfig {
    /// Convert to geyser module's GeyserConfig
    pub fn to_geyser_config(&self) -> crate::geyser::GeyserConfig {
        crate::geyser::GeyserConfig {
            endpoint: self.endpoint.clone(),
            connect_timeout: Duration::from_millis(self.connect_timeout_ms),
            request_timeout: Duration::from_millis(self.request_timeout_ms),
        }
    }
}

/// Local RPC configuration for direct validator access
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalRpcConfig {
    /// Whether to use local RPC instead of QuickNode
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Local RPC URL (default: http://localhost:8899)
    #[serde(default = "default_local_rpc")]
    pub url: String,
    /// Local WebSocket URL (default: ws://localhost:8900)
    #[serde(default = "default_local_ws")]
    pub ws_url: String,
}

fn default_local_rpc() -> String {
    "http://localhost:8899".to_string()
}

fn default_local_ws() -> String {
    "ws://localhost:8900".to_string()
}

impl Default for LocalRpcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            url: default_local_rpc(),
            ws_url: default_local_ws(),
        }
    }
}

impl Config {
    /// Returns the primary RPC URL - prefers local RPC if enabled
    pub fn primary_rpc_url(&self) -> String {
        if self.local_rpc.enabled {
            return self.local_rpc.url.clone();
        }
        self.quicknode_rpcs
            .first()
            .cloned()
            .unwrap_or_else(|| "https://api.mainnet-beta.solana.com".to_string())
    }
    
    /// Returns the WebSocket URL - prefers local if enabled
    pub fn ws_url(&self) -> String {
        if self.local_rpc.enabled {
            return self.local_rpc.ws_url.clone();
        }
        // Convert HTTP URL to WS URL as fallback
        self.primary_rpc_url()
            .replace("https://", "wss://")
            .replace("http://", "ws://")
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WalletsConfig {
    #[serde(default)]
    pub auth: WalletPath,
    #[serde(default)]
    pub payer: WalletPath,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WalletPath {
    pub path: Option<String>,
}

impl WalletPath {
    /// Loads the keypair from the configured path.
    pub fn load_keypair(&self) -> Result<Keypair, Box<dyn std::error::Error>> {
        let path = self.path.as_ref().ok_or("wallet path not configured")?;
        load_keypair_from_file(path)
    }
}

/// Loads a Solana keypair from a JSON file.
pub fn load_keypair_from_file(path: &str) -> Result<Keypair, Box<dyn std::error::Error>> {
    read_keypair_file(path).map_err(|e| format!("failed to read keypair: {:?}", e).into())
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let mut config: serde_json::Value = serde_json::from_str(&content)?;

        // Extract anchor tokens from solend_flash_loans
        let mut anchor_tokens = Vec::new();
        if let Some(solend_loans) = config.get("solend_flash_loans").and_then(|v| v.as_object()) {
            for (_token_name, loan_config) in solend_loans {
                if let Some(mint) = loan_config
                    .get("reserve_liquidity_mint")
                    .and_then(|v| v.as_str())
                {
                    anchor_tokens.push(AnchorToken {
                        mint: mint.to_string(),
                        flash_loan_provider: "solend".to_string(),
                        max_borrow: 1_000_000_000, // 1000 USDC/SOL etc, adjust as needed
                    });
                }
            }
        }

        // Add anchor_tokens to the config
        let anchor_tokens_value = serde_json::to_value(&anchor_tokens)?;
        config["anchor_tokens"] = anchor_tokens_value;

        let config: Config = serde_json::from_value(config)?;
        Ok(config)
    }
}
