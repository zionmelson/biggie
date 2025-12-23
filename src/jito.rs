//! Jito Block Engine client for bundle submission and result streaming.
//!
//! This module provides a gRPC client that connects to Jito's block engine,
//! handles authentication, and submits transaction bundles for MEV extraction.

use anyhow::{anyhow, bail, Result};
use regex::Regex;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::VersionedTransaction;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::Request;
use rustls::RootCertStore;

/// Extracts the instruction index from a simulation failure message.
/// Format: "Error processing Instruction N: ..."
fn extract_instruction_index(msg: &str) -> Option<usize> {
    // Match "Instruction N" or "instruction N" (case insensitive)
    let re = Regex::new(r"(?i)instruction\s+(\d+)").ok()?;
    re.captures(msg)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

/// Extracts the error code from a simulation failure message.
/// Supports formats: "0x1234", "custom program error: 0x1234", decimal codes
fn extract_error_code(msg: &str) -> Option<String> {
    // Try hex format first: 0x followed by hex digits
    let hex_re = Regex::new(r"0x([0-9a-fA-F]+)").ok()?;
    if let Some(caps) = hex_re.captures(msg) {
        if let Some(m) = caps.get(0) {
            return Some(m.as_str().to_string());
        }
    }
    
    // Try "error code: N" format
    let decimal_re = Regex::new(r"error(?:\s+code)?:\s*(\d+)").ok()?;
    if let Some(caps) = decimal_re.captures(msg) {
        if let Some(m) = caps.get(1) {
            let decimal: u32 = m.as_str().parse().ok()?;
            return Some(format!("0x{:x}", decimal));
        }
    }
    
    // Check for common non-program errors
    if msg.contains("Blockhash not found") {
        return Some("BLOCKHASH_NOT_FOUND".to_string());
    }
    if msg.contains("insufficient funds") || msg.contains("InsufficientFunds") {
        return Some("INSUFFICIENT_FUNDS".to_string());
    }
    
    None
}

// Generated protobuf modules
pub mod auth {
    tonic::include_proto!("auth");
}

pub mod bundle {
    tonic::include_proto!("bundle");
}

pub mod packet {
    tonic::include_proto!("packet");
}

pub mod searcher {
    tonic::include_proto!("searcher");
}

pub mod shared {
    tonic::include_proto!("shared");
}

pub mod shredstream {
    tonic::include_proto!("shredstream");
}

use auth::auth_service_client::AuthServiceClient;
use auth::{GenerateAuthChallengeRequest, GenerateAuthTokensRequest, Role};
use bundle::{Bundle, BundleResult};
use packet::{Meta, Packet, PacketFlags};
use searcher::searcher_service_client::SearcherServiceClient;
use searcher::{SendBundleRequest, SubscribeBundleResultsRequest};
use shared::Header;

/// Static list of valid Jito tip accounts.
/// These are the accounts that can receive tips for bundle inclusion.
pub const TIP_ACCOUNTS: &[&str] = &[
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
];

/// Returns a random tip account pubkey for use in bundle tips.
pub fn random_tip_account() -> Pubkey {
    use rand::Rng;
    let idx = rand::thread_rng().gen_range(0..TIP_ACCOUNTS.len());
    Pubkey::from_str(TIP_ACCOUNTS[idx]).expect("invalid tip account")
}

/// Authentication token with expiry tracking.
#[derive(Clone)]
struct AuthToken {
    access_token: String,
    refresh_token: String,
    access_expires: Instant,
    refresh_expires: Instant,
}

impl AuthToken {
    fn is_access_expired(&self) -> bool {
        // Refresh 30 seconds before actual expiry
        self.access_expires
            .saturating_duration_since(Instant::now())
            < Duration::from_secs(30)
    }

    fn is_refresh_expired(&self) -> bool {
        self.refresh_expires
            .saturating_duration_since(Instant::now())
            < Duration::from_secs(30)
    }
}

/// Configuration for the Jito client.
#[derive(Clone, Debug)]
pub struct JitoConfig {
    /// Block engine gRPC endpoint (e.g., "https://ny.mainnet.block-engine.jito.wtf")
    pub endpoint: String,
    /// Optional path to CA certificate for TLS
    pub ca_pem: Option<String>,
}

/// Jito Block Engine client.
///
/// Handles authentication, bundle submission, and result streaming.
pub struct JitoClient {
    #[allow(dead_code)]
    config: JitoConfig,
    auth_keypair: Arc<Keypair>,
    channel: Channel,
    auth_token: Arc<RwLock<Option<AuthToken>>>,
}

impl JitoClient {
    /// Creates a new Jito client and establishes connection.
    pub async fn new(config: JitoConfig, auth_keypair: Arc<Keypair>) -> Result<Self> {
        let channel = Self::create_channel(&config).await?;

        Ok(Self {
            config,
            auth_keypair,
            channel,
            auth_token: Arc::new(RwLock::new(None)),
        })
    }

    /// Creates a gRPC channel with TLS configuration.
    async fn create_channel(config: &JitoConfig) -> Result<Channel> {
        let mut endpoint = Endpoint::from_shared(config.endpoint.clone())?
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10));

        // Configure TLS with proper root certificates
        let tls_config = if let Some(ca_path) = &config.ca_pem {
            let ca_cert = std::fs::read_to_string(ca_path)?;
            let ca = tonic::transport::Certificate::from_pem(&ca_cert);
            ClientTlsConfig::new().ca_certificate(ca)
        } else {
            // Use webpki-roots for standard HTTPS connections
            ClientTlsConfig::new().with_native_roots()
        };

        endpoint = endpoint.tls_config(tls_config)?;

        let channel = endpoint.connect().await?;
        Ok(channel)
    }

    /// Authenticates with the block engine and obtains access tokens.
    pub async fn authenticate(&self) -> Result<()> {
        let mut auth_client = AuthServiceClient::new(self.channel.clone());

        // Step 1: Get challenge
        let challenge_req = GenerateAuthChallengeRequest {
            role: Role::Searcher as i32,
            pubkey: self.auth_keypair.pubkey().to_bytes().to_vec(),
        };

        let challenge_resp = auth_client
            .generate_auth_challenge(Request::new(challenge_req))
            .await?
            .into_inner();

        // Step 2: Sign challenge - format is "{pubkey_base58}-{challenge}"
        let pubkey_b58 = self.auth_keypair.pubkey().to_string();
        let challenge = format!("{}-{}", pubkey_b58, challenge_resp.challenge);
        let signature = self.auth_keypair.sign_message(challenge.as_bytes());

        // Step 3: Get tokens - note: challenge field is the formatted "{pubkey}-{challenge}" string
        let token_req = GenerateAuthTokensRequest {
            challenge,
            client_pubkey: self.auth_keypair.pubkey().to_bytes().to_vec(),
            signed_challenge: signature.as_ref().to_vec(),
        };

        let token_resp = auth_client
            .generate_auth_tokens(Request::new(token_req))
            .await?
            .into_inner();

        let access_token = token_resp
            .access_token
            .ok_or_else(|| anyhow!("no access token in response"))?;
        let refresh_token = token_resp
            .refresh_token
            .ok_or_else(|| anyhow!("no refresh token in response"))?;

        // Parse expiry times
        let access_expires = access_token
            .expires_at_utc
            .map(|ts| {
                let secs = ts.seconds as u64;
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if secs > now_secs {
                    Instant::now() + Duration::from_secs(secs - now_secs)
                } else {
                    Instant::now()
                }
            })
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(300));

        let refresh_expires = refresh_token
            .expires_at_utc
            .map(|ts| {
                let secs = ts.seconds as u64;
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if secs > now_secs {
                    Instant::now() + Duration::from_secs(secs - now_secs)
                } else {
                    Instant::now()
                }
            })
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));

        let auth = AuthToken {
            access_token: access_token.value,
            refresh_token: refresh_token.value,
            access_expires,
            refresh_expires,
        };

        *self.auth_token.write().await = Some(auth);

        Ok(())
    }

    /// Ensures we have a valid access token, refreshing if necessary.
    async fn ensure_authenticated(&self) -> Result<String> {
        let token = self.auth_token.read().await;

        match &*token {
            Some(auth) if !auth.is_access_expired() => {
                return Ok(auth.access_token.clone());
            }
            Some(auth) if !auth.is_refresh_expired() => {
                // Need to refresh - drop read lock first
                let refresh_token = auth.refresh_token.clone();
                drop(token);
                return self.refresh_access_token(&refresh_token).await;
            }
            _ => {
                // Need full re-auth
                drop(token);
                self.authenticate().await?;
                let token = self.auth_token.read().await;
                return Ok(token
                    .as_ref()
                    .ok_or_else(|| anyhow!("auth failed"))?
                    .access_token
                    .clone());
            }
        }
    }

    /// Refreshes the access token using the refresh token.
    async fn refresh_access_token(&self, refresh_token: &str) -> Result<String> {
        let mut auth_client = AuthServiceClient::new(self.channel.clone());

        let req = auth::RefreshAccessTokenRequest {
            refresh_token: refresh_token.to_string(),
        };

        let resp = auth_client
            .refresh_access_token(Request::new(req))
            .await?
            .into_inner();

        let new_access = resp
            .access_token
            .ok_or_else(|| anyhow!("no access token in refresh response"))?;

        let access_expires = new_access
            .expires_at_utc
            .map(|ts| {
                let secs = ts.seconds as u64;
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if secs > now_secs {
                    Instant::now() + Duration::from_secs(secs - now_secs)
                } else {
                    Instant::now()
                }
            })
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(300));

        let mut token = self.auth_token.write().await;
        if let Some(ref mut auth) = *token {
            auth.access_token = new_access.value.clone();
            auth.access_expires = access_expires;
        }

        Ok(new_access.value)
    }

    /// Creates an authenticated request with the access token.
    fn authenticated_request<T>(&self, inner: T, access_token: &str) -> Request<T> {
        let mut request = Request::new(inner);
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", access_token).parse().unwrap(),
        );
        request
    }

    /// Converts a serialized transaction to a Jito packet.
    fn tx_to_packet(tx_bytes: &[u8]) -> Packet {
        Packet {
            data: tx_bytes.to_vec(),
            meta: Some(Meta {
                size: tx_bytes.len() as u64,
                addr: String::new(),
                port: 0,
                flags: Some(PacketFlags {
                    discard: false,
                    forwarded: false,
                    repair: false,
                    simple_vote_tx: false,
                    tracer_packet: false,
                    from_staked_node: false,
                }),
                sender_stake: 0,
            }),
        }
    }

    /// Sends a bundle of transactions to the block engine.
    ///
    /// Returns the bundle UUID on success.
    pub async fn send_bundle(&self, transactions: Vec<Vec<u8>>) -> Result<String> {
        if transactions.is_empty() {
            bail!("cannot send empty bundle");
        }

        if transactions.len() > 5 {
            bail!("bundle exceeds maximum of 5 transactions");
        }

        let access_token = self.ensure_authenticated().await?;
        let mut client = SearcherServiceClient::new(self.channel.clone());

        let packets: Vec<Packet> = transactions
            .iter()
            .map(|tx| Self::tx_to_packet(tx))
            .collect();

        let bundle = Bundle {
            header: Some(Header {
                ts: Some(prost_types::Timestamp::from(std::time::SystemTime::now())),
            }),
            packets,
        };

        let request = self.authenticated_request(
            SendBundleRequest {
                bundle: Some(bundle),
            },
            &access_token,
        );

        let response = client.send_bundle(request).await?.into_inner();

        Ok(response.uuid)
    }

    /// Sends a bundle of versioned transactions.
    pub async fn send_versioned_bundle(
        &self,
        transactions: Vec<VersionedTransaction>,
    ) -> Result<String> {
        let serialized: Result<Vec<Vec<u8>>> = transactions
            .into_iter()
            .map(|tx| bincode::serialize(&tx).map_err(|e| anyhow!("failed to serialize tx: {}", e)))
            .collect();

        self.send_bundle(serialized?).await
    }

    /// Subscribes to bundle results for this searcher.
    ///
    /// Returns a stream of bundle results.
    pub async fn subscribe_bundle_results(&self) -> Result<tonic::Streaming<BundleResult>> {
        let access_token = self.ensure_authenticated().await?;
        let mut client = SearcherServiceClient::new(self.channel.clone());

        let request = self.authenticated_request(SubscribeBundleResultsRequest {}, &access_token);

        let response = client.subscribe_bundle_results(request).await?;

        Ok(response.into_inner())
    }

    /// Gets the next scheduled leader information.
    pub async fn get_next_scheduled_leader(&self) -> Result<searcher::NextScheduledLeaderResponse> {
        let access_token = self.ensure_authenticated().await?;
        let mut client = SearcherServiceClient::new(self.channel.clone());

        let request = self.authenticated_request(
            searcher::NextScheduledLeaderRequest { regions: vec![] },
            &access_token,
        );

        let response = client
            .get_next_scheduled_leader(request)
            .await?
            .into_inner();

        Ok(response)
    }
}

/// Bundle result types for easier handling.
#[derive(Debug, Clone)]
pub enum BundleStatus {
    /// Bundle was submitted but no result yet
    Submitted { bundle_id: String },
    /// Bundle was accepted and forwarded to a validator
    Accepted {
        bundle_id: String,
        slot: u64,
        validator: String,
    },
    /// Bundle was rejected by the block engine
    Rejected { bundle_id: String, reason: String },
    /// Bundle reached finalized commitment
    Finalized { bundle_id: String },
    /// Bundle was processed (but not yet finalized)
    Processed {
        bundle_id: String,
        slot: u64,
        validator: String,
    },
    /// Bundle was dropped after being accepted
    Dropped { bundle_id: String, reason: String },
    /// Unknown result type
    Unknown { bundle_id: String },
}

impl From<BundleResult> for BundleStatus {
    fn from(result: BundleResult) -> Self {
        use bundle::bundle_result::Result as BR;
        use bundle::DroppedReason;

        let bundle_id = result.bundle_id;

        match result.result {
            Some(BR::Accepted(a)) => BundleStatus::Accepted {
                bundle_id: bundle_id.clone(),
                slot: a.slot,
                validator: a.validator_identity,
            },
            Some(BR::Rejected(r)) => {
                let reason = if let Some(reason) = r.reason {
                    use bundle::rejected::Reason;
                    match reason {
                        Reason::StateAuctionBidRejected(s) => {
                            format!("state auction bid rejected: {:?}", s.msg)
                        }
                        Reason::WinningBatchBidRejected(w) => {
                            format!("winning batch bid rejected: {:?}", w.msg)
                        }
                        Reason::SimulationFailure(s) => {
                            // Parse the message for structured error info
                            let msg_str = s.msg.clone().unwrap_or_default();
                            
                            // Extract instruction index and error code from message
                            // Format: "Error processing Instruction N: custom program error: 0xXXXX"
                            let ix_index = extract_instruction_index(&msg_str);
                            let error_code = extract_error_code(&msg_str);
                            
                            format!(
                                "SIMULATION_FAILURE | sig={} | ix={} | error={} | raw={}",
                                &s.tx_signature[..16],  // Short sig for readability
                                ix_index.map(|i| i.to_string()).unwrap_or_else(|| "?".to_string()),
                                error_code.unwrap_or_else(|| "unknown".to_string()),
                                msg_str.chars().take(200).collect::<String>()  // Truncate raw msg
                            )
                        }
                        Reason::InternalError(e) => format!("internal error: {}", e.msg),
                        Reason::DroppedBundle(d) => format!("dropped: {}", d.msg),
                    }
                } else {
                    "unknown rejection reason".to_string()
                };
                BundleStatus::Rejected {
                    bundle_id: bundle_id.clone(),
                    reason,
                }
            }
            Some(BR::Finalized(_)) => BundleStatus::Finalized {
                bundle_id: bundle_id.clone(),
            },
            Some(BR::Processed(p)) => BundleStatus::Processed {
                bundle_id: bundle_id.clone(),
                slot: p.slot,
                validator: p.validator_identity,
            },
            Some(BR::Dropped(d)) => {
                let reason = match d.reason {
                    r if r == DroppedReason::BlockhashExpired as i32 => "blockhash expired",
                    r if r == DroppedReason::PartiallyProcessed as i32 => "partially processed",
                    r if r == DroppedReason::NotFinalized as i32 => "not finalized",
                    _ => "unknown",
                };
                BundleStatus::Dropped {
                    bundle_id: bundle_id.clone(),
                    reason: reason.to_string(),
                }
            }
            None => BundleStatus::Unknown { bundle_id },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tip_accounts_valid() {
        for account in TIP_ACCOUNTS {
            let pubkey = Pubkey::from_str(account);
            assert!(pubkey.is_ok(), "invalid tip account: {}", account);
        }
    }

    #[test]
    fn test_random_tip_account() {
        let tip = random_tip_account();
        let tip_str = tip.to_string();
        assert!(TIP_ACCOUNTS.contains(&tip_str.as_str()));
    }
}
