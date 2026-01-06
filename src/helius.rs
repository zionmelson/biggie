//! Helius WebSocket + RPC client
//!
//! Single connection, simple interface. No pooling complexity.

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_tungstenite::{
    connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

/// Account update notification from Helius WebSocket
#[derive(Debug, Clone)]
pub struct AccountUpdate {
    pub pubkey: Pubkey,
    pub data: Vec<u8>,
    pub slot: u64,
}

/// Helius client for WebSocket subscriptions and RPC calls
pub struct Helius {
    rpc_url: String,
    ws_url: String,
    ws: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    subscriptions: HashMap<Pubkey, u64>, // pubkey -> subscription_id
    request_id: AtomicU64,
    http_client: reqwest::Client,
    /// Cached latest blockhash
    blockhash: Arc<RwLock<(Hash, u64)>>, // (hash, slot)
}

impl Helius {
    pub fn new(rpc_url: String, ws_url: String) -> Self {
        Self {
            rpc_url,
            ws_url,
            ws: None,
            subscriptions: HashMap::new(),
            request_id: AtomicU64::new(1),
            http_client: reqwest::Client::new(),
            blockhash: Arc::new(RwLock::new((Hash::default(), 0))),
        }
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Connect to WebSocket
    pub async fn connect(&mut self) -> Result<()> {
        tracing::info!("Connecting to Helius WebSocket: {}", self.ws_url);
        let (ws, _) = connect_async(&self.ws_url).await?;
        self.ws = Some(ws);
        tracing::info!("Helius WebSocket connected");
        Ok(())
    }

    /// Subscribe with proper ID tracking
    pub async fn subscribe_accounts_tracked(&mut self, pubkeys: &[Pubkey]) -> Result<()> {
        // Build all requests first to avoid borrow conflicts
        let requests: Vec<(u64, Pubkey, String)> = pubkeys.iter().map(|pubkey| {
            let id = self.next_id();
            let request = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "accountSubscribe",
                "params": [
                    pubkey.to_string(),
                    { "encoding": "base64", "commitment": "confirmed" }
                ]
            });
            (id, *pubkey, request.to_string())
        }).collect();

        let mut pending: HashMap<u64, Pubkey> = HashMap::new();
        
        // Send all requests
        {
            let ws = self.ws.as_mut().ok_or_else(|| anyhow!("WebSocket not connected"))?;
            for (id, pubkey, request) in requests {
                pending.insert(id, pubkey);
                ws.send(Message::Text(request)).await?;
            }
        }

        // Wait for subscription confirmations
        let mut confirmed = 0;
        while confirmed < pubkeys.len() {
            let msg = {
                let ws = self.ws.as_mut().ok_or_else(|| anyhow!("WebSocket not connected"))?;
                ws.next().await
            };
            
            if let Some(Ok(Message::Text(text))) = msg {
                let parsed: Value = serde_json::from_str(&text)?;
                if let (Some(id), Some(result)) = (
                    parsed.get("id").and_then(|i| i.as_u64()),
                    parsed.get("result").and_then(|r| r.as_u64())
                ) {
                    if let Some(pubkey) = pending.remove(&id) {
                        self.subscriptions.insert(pubkey, result);
                        confirmed += 1;
                    }
                }
            }
        }

        tracing::info!("Confirmed {} account subscriptions", confirmed);
        Ok(())
    }

    /// Reverse lookup: subscription_id -> pubkey
    fn pubkey_for_subscription(&self, sub_id: u64) -> Option<Pubkey> {
        self.subscriptions.iter()
            .find(|(_, &id)| id == sub_id)
            .map(|(pk, _)| *pk)
    }

    /// Wait for next account update (with proper pubkey resolution)
    pub async fn recv_update(&mut self) -> Result<AccountUpdate> {
        loop {
            let msg = {
                let ws = self.ws.as_mut().ok_or_else(|| anyhow!("WebSocket not connected"))?;
                ws.next().await
            };
            
            match msg {
                Some(Ok(Message::Text(text))) => {
                    let parsed: Value = serde_json::from_str(&text)?;
                    
                    if parsed.get("method").and_then(|m| m.as_str()) == Some("accountNotification") {
                        let params = parsed.get("params").ok_or_else(|| anyhow!("No params"))?;
                        let sub_id = params.get("subscription").and_then(|s| s.as_u64()).unwrap_or(0);
                        
                        let pubkey = self.pubkey_for_subscription(sub_id)
                            .ok_or_else(|| anyhow!("Unknown subscription: {}", sub_id))?;

                        let result = params.get("result").ok_or_else(|| anyhow!("No result"))?;
                        let context = result.get("context").ok_or_else(|| anyhow!("No context"))?;
                        let slot = context.get("slot").and_then(|s| s.as_u64()).unwrap_or(0);

                        let value = result.get("value").ok_or_else(|| anyhow!("No value"))?;
                        let data_arr = value.get("data").ok_or_else(|| anyhow!("No data"))?;
                        let base64_data = data_arr.get(0).and_then(|d| d.as_str())
                            .ok_or_else(|| anyhow!("No data string"))?;
                        
                        let data = base64::Engine::decode(
                            &base64::engine::general_purpose::STANDARD, 
                            base64_data
                        )?;

                        return Ok(AccountUpdate { pubkey, data, slot });
                    }
                }
                Some(Ok(Message::Ping(data))) => {
                    let ws = self.ws.as_mut().ok_or_else(|| anyhow!("WebSocket not connected"))?;
                    ws.send(Message::Pong(data)).await?;
                }
                Some(Ok(Message::Close(_))) => {
                    return Err(anyhow!("WebSocket closed"));
                }
                Some(Err(e)) => {
                    return Err(anyhow!("WebSocket error: {}", e));
                }
                None => {
                    return Err(anyhow!("WebSocket stream ended"));
                }
                _ => {}
            }
        }
    }

    // ========== RPC Methods ==========

    async fn rpc_call(&self, method: &str, params: Value) -> Result<Value> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": method,
            "params": params
        });

        let response: Value = self.http_client
            .post(&self.rpc_url)
            .json(&request)
            .send()
            .await?
            .json()
            .await?;

        if let Some(error) = response.get("error") {
            return Err(anyhow!("RPC error: {}", error));
        }

        response.get("result").cloned().ok_or_else(|| anyhow!("No result"))
    }

    /// Get latest blockhash
    pub async fn get_latest_blockhash(&self) -> Result<Hash> {
        let result = self.rpc_call("getLatestBlockhash", json!([
            { "commitment": "confirmed" }
        ])).await?;

        let value = result.get("value").ok_or_else(|| anyhow!("No value"))?;
        let blockhash_str = value.get("blockhash").and_then(|b| b.as_str())
            .ok_or_else(|| anyhow!("No blockhash"))?;

        Ok(Hash::from_str(blockhash_str)?)
    }

    /// Refresh blockhash cache
    pub async fn refresh_blockhash(&self) -> Result<Hash> {
        let hash = self.get_latest_blockhash().await?;
        let mut cache = self.blockhash.write().await;
        *cache = (hash, 0);
        Ok(hash)
    }

    /// Get blockhash cache handle for sharing
    pub fn blockhash_cache(&self) -> Arc<RwLock<(Hash, u64)>> {
        self.blockhash.clone()
    }
}

/// Parse SPL Token account balance from account data
/// SPL Token account layout: 165 bytes
/// - mint: 32 bytes (offset 0)
/// - owner: 32 bytes (offset 32)
/// - amount: 8 bytes (offset 64) <-- what we need
pub fn parse_token_balance(data: &[u8]) -> Option<u64> {
    if data.len() < 72 {
        return None;
    }
    // Amount is at offset 64, 8 bytes little-endian
    let amount_bytes: [u8; 8] = data[64..72].try_into().ok()?;
    Some(u64::from_le_bytes(amount_bytes))
}
