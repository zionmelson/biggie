//! Helius WebSocket + RPC client
//!
//! Multi-connection design: splits subscriptions across multiple WebSocket connections
//! to stay under Helius's per-connection subscription limit (~100).

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
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::{
    connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

/// Maximum subscriptions per WebSocket connection (Helius limit is ~100)
const MAX_SUBS_PER_CONNECTION: usize = 90;

/// Account update notification from Helius WebSocket
#[derive(Debug, Clone)]
pub struct AccountUpdate {
    pub pubkey: Pubkey,
    pub data: Vec<u8>,
    pub slot: u64,
}

/// Single WebSocket connection with its subscriptions
struct WsConnection {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    subscriptions: HashMap<Pubkey, u64>, // pubkey -> subscription_id
}

impl WsConnection {
    /// Reverse lookup: subscription_id -> pubkey
    fn pubkey_for_subscription(&self, sub_id: u64) -> Option<Pubkey> {
        self.subscriptions.iter()
            .find(|(_, &id)| id == sub_id)
            .map(|(pk, _)| *pk)
    }
}

/// Helius client for WebSocket subscriptions and RPC calls
/// Manages multiple WebSocket connections to work around subscription limits
pub struct Helius {
    rpc_url: String,
    ws_url: String,
    request_id: AtomicU64,
    http_client: reqwest::Client,
    /// Cached latest blockhash
    blockhash: Arc<RwLock<(Hash, u64)>>,
    /// Channel to receive updates from all connections
    update_rx: Option<mpsc::Receiver<AccountUpdate>>,
    /// Sender for spawning additional connections
    update_tx: Option<mpsc::Sender<AccountUpdate>>,
    /// Handle to spawned WebSocket tasks
    _ws_handles: Vec<tokio::task::JoinHandle<()>>,
}

impl Helius {
    pub fn new(rpc_url: String, ws_url: String) -> Self {
        Self {
            rpc_url,
            ws_url,
            request_id: AtomicU64::new(1),
            http_client: reqwest::Client::new(),
            blockhash: Arc::new(RwLock::new((Hash::default(), 0))),
            update_rx: None,
            update_tx: None,
            _ws_handles: Vec::new(),
        }
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Connect is now a no-op - connections are established per subscription batch
    pub async fn connect(&mut self) -> Result<()> {
        tracing::info!("Helius multi-connection mode ready (max {} subs/connection)", MAX_SUBS_PER_CONNECTION);
        Ok(())
    }

    /// Subscribe to accounts, splitting across multiple connections if needed
    pub async fn subscribe_accounts_tracked(&mut self, pubkeys: &[Pubkey]) -> Result<()> {
        if pubkeys.is_empty() {
            return Ok(());
        }

        // Calculate number of connections needed
        let num_connections = (pubkeys.len() + MAX_SUBS_PER_CONNECTION - 1) / MAX_SUBS_PER_CONNECTION;
        tracing::info!(
            "Subscribing to {} accounts across {} connection(s) ({} per connection max)",
            pubkeys.len(),
            num_connections,
            MAX_SUBS_PER_CONNECTION
        );

        // Create or reuse channel for aggregating updates from all connections
        let tx = if let Some(tx) = &self.update_tx {
            tx.clone()
        } else {
            let (tx, rx) = mpsc::channel::<AccountUpdate>(10000);
            self.update_rx = Some(rx);
            self.update_tx = Some(tx.clone());
            tx
        };

        // Split pubkeys into chunks for each connection
        let chunks: Vec<Vec<Pubkey>> = pubkeys
            .chunks(MAX_SUBS_PER_CONNECTION)
            .map(|c| c.to_vec())
            .collect();

        // Spawn a task for each connection
        for (i, chunk) in chunks.into_iter().enumerate() {
            let ws_url = self.ws_url.clone();
            let tx = tx.clone();
            let conn_id = self._ws_handles.len() + i + 1;
            
            let handle = tokio::spawn(async move {
                loop {
                    match run_connection(conn_id, ws_url.clone(), chunk.clone(), tx.clone()).await {
                        Ok(()) => break, // Clean exit
                        Err(e) => {
                            tracing::error!("Connection {} error: {}. Reconnecting in 5s...", conn_id, e);
                            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        }
                    }
                }
            });
            self._ws_handles.push(handle);
        }

        // Wait a bit for connections to establish and subscribe
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        
        Ok(())
    }

    /// Wait for next account update from any connection
    pub async fn recv_update(&mut self) -> Result<AccountUpdate> {
        let rx = self.update_rx.as_mut()
            .ok_or_else(|| anyhow!("No subscriptions active"))?;
        
        rx.recv().await
            .ok_or_else(|| anyhow!("All WebSocket connections closed"))
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

/// Run a single WebSocket connection, subscribing to the given pubkeys
/// and forwarding updates to the channel
async fn run_connection(
    conn_id: usize,
    ws_url: String,
    pubkeys: Vec<Pubkey>,
    tx: mpsc::Sender<AccountUpdate>,
) -> Result<()> {
    tracing::info!("Connection {}: connecting to subscribe to {} accounts...", conn_id, pubkeys.len());
    
    // Connect
    let (ws, _) = connect_async(&ws_url).await?;
    tracing::info!("Connection {}: WebSocket connected", conn_id);
    
    let mut conn = WsConnection {
        ws,
        subscriptions: HashMap::new(),
    };

    // Subscribe to all accounts with rate limiting
    subscribe_accounts(&mut conn, conn_id, &pubkeys).await?;
    
    tracing::info!("Connection {}: {} subscriptions active, listening for updates...", 
        conn_id, conn.subscriptions.len());

    // Main loop: receive updates and forward to channel
    loop {
        match conn.ws.next().await {
            Some(Ok(Message::Text(text))) => {
                if let Some(update) = parse_notification(&text, &conn) {
                    if tx.send(update).await.is_err() {
                        tracing::warn!("Connection {}: update channel closed", conn_id);
                        break;
                    }
                }
            }
            Some(Ok(Message::Ping(data))) => {
                conn.ws.send(Message::Pong(data)).await?;
            }
            Some(Ok(Message::Close(frame))) => {
                let reason = frame.map(|f| format!("code={}, reason={}", f.code, f.reason))
                    .unwrap_or_else(|| "no frame".to_string());
                tracing::error!("Connection {}: closed by server: {}", conn_id, reason);
                return Err(anyhow!("Connection closed: {}", reason));
            }
            Some(Err(e)) => {
                tracing::error!("Connection {}: error: {}", conn_id, e);
                return Err(anyhow!("WebSocket error: {}", e));
            }
            None => {
                tracing::error!("Connection {}: stream ended", conn_id);
                return Err(anyhow!("Stream ended"));
            }
            _ => {}
        }
    }

    Ok(())
}

/// Subscribe to accounts on a connection with rate limiting
async fn subscribe_accounts(
    conn: &mut WsConnection,
    conn_id: usize,
    pubkeys: &[Pubkey],
) -> Result<()> {
    const BATCH_SIZE: usize = 10;
    const BATCH_DELAY_MS: u64 = 100;
    
    let mut request_id = 1u64;
    let mut pending: HashMap<u64, Pubkey> = HashMap::new();
    
    // Send subscription requests in batches
    for (batch_idx, chunk) in pubkeys.chunks(BATCH_SIZE).enumerate() {
        for pubkey in chunk {
            let id = request_id;
            request_id += 1;
            
            let request = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "accountSubscribe",
                "params": [
                    pubkey.to_string(),
                    { "encoding": "base64", "commitment": "confirmed" }
                ]
            });
            
            pending.insert(id, *pubkey);
            conn.ws.send(Message::Text(request.to_string())).await?;
        }
        
        // Progress logging
        let sent = (batch_idx + 1) * BATCH_SIZE;
        if sent <= pubkeys.len() && sent % 30 == 0 {
            tracing::debug!("Connection {}: sent {}/{} subscription requests", 
                conn_id, sent.min(pubkeys.len()), pubkeys.len());
        }
        
        // Drain responses and pings between batches
        drain_and_process(&mut conn.ws, &mut pending, &mut conn.subscriptions).await?;
        
        if (batch_idx + 1) * BATCH_SIZE < pubkeys.len() {
            tokio::time::sleep(tokio::time::Duration::from_millis(BATCH_DELAY_MS)).await;
        }
    }
    
    // Wait for remaining confirmations
    let timeout = tokio::time::Duration::from_secs(30);
    let start = tokio::time::Instant::now();
    
    while !pending.is_empty() && start.elapsed() < timeout {
        match tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            conn.ws.next()
        ).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                    if let Some(error) = parsed.get("error") {
                        tracing::error!("Connection {}: subscription error: {}", conn_id, error);
                        return Err(anyhow!("Subscription error: {}", error));
                    }
                    
                    if let (Some(id), Some(result)) = (
                        parsed.get("id").and_then(|i| i.as_u64()),
                        parsed.get("result").and_then(|r| r.as_u64())
                    ) {
                        if let Some(pubkey) = pending.remove(&id) {
                            conn.subscriptions.insert(pubkey, result);
                        }
                    }
                }
            }
            Ok(Some(Ok(Message::Ping(data)))) => {
                conn.ws.send(Message::Pong(data)).await?;
            }
            Ok(Some(Ok(Message::Close(frame)))) => {
                let reason = frame.map(|f| format!("code={}, reason={}", f.code, f.reason))
                    .unwrap_or_else(|| "no frame".to_string());
                return Err(anyhow!("Connection {} closed during subscription: {}", conn_id, reason));
            }
            Ok(Some(Err(e))) => {
                return Err(anyhow!("Connection {} error during subscription: {}", conn_id, e));
            }
            Ok(None) => {
                return Err(anyhow!("Connection {} stream ended during subscription", conn_id));
            }
            Err(_) => {
                // Timeout, continue waiting
            }
            _ => {}
        }
    }
    
    if !pending.is_empty() {
        tracing::warn!("Connection {}: {} subscriptions timed out", conn_id, pending.len());
    }
    
    tracing::info!("Connection {}: confirmed {} subscriptions", conn_id, conn.subscriptions.len());
    Ok(())
}

/// Drain incoming messages and process subscription confirmations (non-blocking)
async fn drain_and_process(
    ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    pending: &mut HashMap<u64, Pubkey>,
    subscriptions: &mut HashMap<Pubkey, u64>,
) -> Result<()> {
    loop {
        match tokio::time::timeout(
            tokio::time::Duration::from_millis(5),
            ws.next()
        ).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                    if let (Some(id), Some(result)) = (
                        parsed.get("id").and_then(|i| i.as_u64()),
                        parsed.get("result").and_then(|r| r.as_u64())
                    ) {
                        if let Some(pubkey) = pending.remove(&id) {
                            subscriptions.insert(pubkey, result);
                        }
                    }
                }
            }
            Ok(Some(Ok(Message::Ping(data)))) => {
                ws.send(Message::Pong(data)).await?;
            }
            Ok(Some(Ok(Message::Close(frame)))) => {
                let reason = frame.map(|f| format!("code={}, reason={}", f.code, f.reason))
                    .unwrap_or_else(|| "no frame".to_string());
                return Err(anyhow!("WebSocket closed: {}", reason));
            }
            Ok(Some(Err(e))) => {
                return Err(anyhow!("WebSocket error: {}", e));
            }
            Ok(None) => {
                return Err(anyhow!("WebSocket stream ended"));
            }
            Err(_) => {
                // Timeout = no messages waiting
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Parse an account notification message
fn parse_notification(text: &str, conn: &WsConnection) -> Option<AccountUpdate> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    
    if parsed.get("method").and_then(|m| m.as_str()) != Some("accountNotification") {
        return None;
    }
    
    let params = parsed.get("params")?;
    let sub_id = params.get("subscription").and_then(|s| s.as_u64())?;
    
    let pubkey = conn.pubkey_for_subscription(sub_id)?;
    
    let result = params.get("result")?;
    let context = result.get("context")?;
    let slot = context.get("slot").and_then(|s| s.as_u64()).unwrap_or(0);
    
    let value = result.get("value")?;
    let data_arr = value.get("data")?;
    let base64_data = data_arr.get(0).and_then(|d| d.as_str())?;
    
    let data = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        base64_data
    ).ok()?;
    
    Some(AccountUpdate { pubkey, data, slot })
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
