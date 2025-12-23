//! Geyser gRPC Client for Local Validator
//!
//! Connects to the local validator's Geyser plugin for ultra-low-latency
//! transaction and block streaming. This replaces external data sources
//! (CoreCast, shredstream proxy) with direct validator access.
//!
//! Available streams:
//! - TransactionUpdates: Full transaction data as processed
//! - SlotUpdates: Slot status (FIRST_SHRED_RECEIVED, PROCESSED, CONFIRMED, ROOTED)
//! - BlockUpdates: Block completion with blockhash
//! - AccountUpdates: Real-time account changes
//! - ProgramUpdates: Account changes filtered by program owner

use anyhow::{Context, Result};
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::transport::Channel;

// Generated protobuf types from geyser protos
pub mod proto {
    // Storage types (confirmed_block, entries, etc.)
    pub mod storage {
        #![allow(clippy::all)]
        
        pub mod confirmed_block {
            tonic::include_proto!("solana.storage.confirmed_block");
        }
        
        pub mod entries {
            tonic::include_proto!("solana.storage.entries");
        }
    }
    
    // Geyser service
    pub mod geyser {
        #![allow(clippy::all)]
        // Re-export storage types so geyser can reference them
        pub use super::storage;
        tonic::include_proto!("solana.geyser");
    }
}

use proto::geyser::geyser_client::GeyserClient as GeyserGrpcClient;
use proto::geyser::{
    EmptyRequest, SubscribeSlotUpdateRequest, SubscribeTransactionUpdatesRequest,
    SubscribeBlockUpdatesRequest, SubscribeProgramsUpdatesRequest,
    TimestampedTransactionUpdate, TimestampedSlotUpdate, TimestampedBlockUpdate,
    SlotUpdateStatus,
};

/// Configuration for the Geyser client
#[derive(Clone, Debug)]
pub struct GeyserConfig {
    /// Geyser gRPC endpoint (default: http://localhost:10000)
    pub endpoint: String,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Request timeout
    pub request_timeout: Duration,
}

impl Default for GeyserConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:10000".to_string(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// Blockhash update from Geyser
#[derive(Clone, Debug)]
pub struct BlockhashUpdate {
    pub hash: Hash,
    pub slot: u64,
}

/// Slot status update from Geyser
#[derive(Clone, Debug)]
pub struct SlotUpdate {
    pub slot: u64,
    pub parent_slot: Option<u64>,
    pub status: SlotStatus,
}

/// Slot status enum matching Geyser's SlotUpdateStatus
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotStatus {
    FirstShredReceived,
    Completed,
    CreatedBank,
    Processed,
    Confirmed,
    Rooted,
    Dead,
}

impl From<i32> for SlotStatus {
    fn from(value: i32) -> Self {
        match value {
            0 => SlotStatus::Confirmed,
            1 => SlotStatus::Processed,
            2 => SlotStatus::Rooted,
            3 => SlotStatus::FirstShredReceived,
            4 => SlotStatus::Completed,
            5 => SlotStatus::CreatedBank,
            6 => SlotStatus::Dead,
            _ => SlotStatus::Processed,
        }
    }
}

/// Transaction update from Geyser (includes full tx data)
#[derive(Clone, Debug)]
pub struct TransactionUpdate {
    /// Slot where the transaction was processed
    pub slot: u64,
    /// Transaction signature (base58)
    pub signature: String,
    /// Whether this is a vote transaction
    pub is_vote: bool,
    /// Transaction index within the slot
    pub tx_idx: u64,
    /// The full confirmed transaction with metadata
    pub transaction: Option<proto::storage::confirmed_block::ConfirmedTransaction>,
}

impl From<proto::geyser::TransactionUpdate> for TransactionUpdate {
    fn from(update: proto::geyser::TransactionUpdate) -> Self {
        Self {
            slot: update.slot,
            signature: update.signature,
            is_vote: update.is_vote,
            tx_idx: update.tx_idx,
            transaction: update.tx,
        }
    }
}

/// Geyser client for streaming data from local validator
pub struct GeyserClient {
    config: GeyserConfig,
    channel: Channel,
}

impl GeyserClient {
    /// Create a new Geyser client and connect
    pub async fn new(config: GeyserConfig) -> Result<Self> {
        let channel = Channel::from_shared(config.endpoint.clone())
            .context("Invalid Geyser endpoint")?
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .connect()
            .await
            .context("Failed to connect to Geyser gRPC")?;

        tracing::info!("Connected to Geyser at {}", config.endpoint);

        Ok(Self { config, channel })
    }

    /// Connect with default configuration (localhost:10000)
    pub async fn connect_local() -> Result<Self> {
        Self::new(GeyserConfig::default()).await
    }

    /// Get heartbeat interval from server
    pub async fn get_heartbeat_interval(&self) -> Result<u64> {
        let mut client = GeyserGrpcClient::new(self.channel.clone());
        let response = client
            .get_heartbeat_interval(EmptyRequest {})
            .await?
            .into_inner();
        Ok(response.heartbeat_interval_ms)
    }

    /// Subscribe to transaction updates (non-vote transactions)
    /// 
    /// This is the primary stream for MEV - gives us full transaction data
    /// as transactions are processed by the validator.
    pub async fn subscribe_transactions(
        &self,
    ) -> Result<tonic::Streaming<TimestampedTransactionUpdate>> {
        let mut client = GeyserGrpcClient::new(self.channel.clone());
        let request = tonic::Request::new(SubscribeTransactionUpdatesRequest {});
        let response = client.subscribe_transaction_updates(request).await?;
        Ok(response.into_inner())
    }

    /// Subscribe to slot updates
    /// 
    /// Useful for tracking slot timing (FIRST_SHRED_RECEIVED is earliest signal)
    pub async fn subscribe_slots(&self) -> Result<tonic::Streaming<TimestampedSlotUpdate>> {
        let mut client = GeyserGrpcClient::new(self.channel.clone());
        let request = tonic::Request::new(SubscribeSlotUpdateRequest {});
        let response = client.subscribe_slot_updates(request).await?;
        Ok(response.into_inner())
    }

    /// Subscribe to block updates
    /// 
    /// Provides blockhash immediately when slot completes
    pub async fn subscribe_blocks(&self) -> Result<tonic::Streaming<TimestampedBlockUpdate>> {
        let mut client = GeyserGrpcClient::new(self.channel.clone());
        let request = tonic::Request::new(SubscribeBlockUpdatesRequest {});
        let response = client.subscribe_block_updates(request).await?;
        Ok(response.into_inner())
    }

    /// Subscribe to program account updates
    /// 
    /// For tracking DEX pool state changes in real-time
    pub async fn subscribe_programs(
        &self,
        program_ids: Vec<Pubkey>,
    ) -> Result<tonic::Streaming<proto::geyser::TimestampedAccountUpdate>> {
        let mut client = GeyserGrpcClient::new(self.channel.clone());
        let programs: Vec<Vec<u8>> = program_ids.iter().map(|p| p.to_bytes().to_vec()).collect();
        let request = tonic::Request::new(SubscribeProgramsUpdatesRequest { programs });
        let response = client.subscribe_program_updates(request).await?;
        Ok(response.into_inner())
    }
}

/// Spawn a background task that streams blockhash updates
pub async fn spawn_blockhash_stream(
    config: GeyserConfig,
    tx: mpsc::Sender<BlockhashUpdate>,
) -> Result<()> {
    tokio::spawn(async move {
        loop {
            match run_blockhash_stream(&config, &tx).await {
                Ok(()) => {
                    tracing::info!("Blockhash stream ended normally");
                    break;
                }
                Err(e) => {
                    tracing::warn!("Blockhash stream error: {}, reconnecting in 3s...", e);
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    });
    Ok(())
}

async fn run_blockhash_stream(
    config: &GeyserConfig,
    tx: &mpsc::Sender<BlockhashUpdate>,
) -> Result<()> {
    let client = GeyserClient::new(config.clone()).await?;
    let mut stream = client.subscribe_blocks().await?;

    tracing::info!("Geyser blockhash stream connected");

    while let Some(update) = stream.message().await? {
        if let Some(block) = update.block_update {
            // Parse blockhash from base58 string
            if let Ok(hash) = block.blockhash.parse::<Hash>() {
                let update = BlockhashUpdate {
                    hash,
                    slot: block.slot,
                };
                if tx.send(update).await.is_err() {
                    tracing::warn!("Blockhash channel closed");
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Spawn a background task that streams transaction updates
pub async fn spawn_transaction_stream(
    config: GeyserConfig,
    tx: mpsc::Sender<TransactionUpdate>,
    skip_votes: bool,
) -> Result<()> {
    tokio::spawn(async move {
        loop {
            match run_transaction_stream(&config, &tx, skip_votes).await {
                Ok(()) => {
                    tracing::info!("Transaction stream ended normally");
                    break;
                }
                Err(e) => {
                    tracing::warn!("Transaction stream error: {}, reconnecting in 3s...", e);
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    });
    Ok(())
}

async fn run_transaction_stream(
    config: &GeyserConfig,
    tx: &mpsc::Sender<TransactionUpdate>,
    skip_votes: bool,
) -> Result<()> {
    let client = GeyserClient::new(config.clone()).await?;
    let mut stream = client.subscribe_transactions().await?;

    tracing::info!("Geyser transaction stream connected (skip_votes={})", skip_votes);

    while let Some(update) = stream.message().await? {
        if let Some(tx_update) = update.transaction {
            // Skip vote transactions if requested (they're ~90% of traffic)
            if skip_votes && tx_update.is_vote {
                continue;
            }

            let converted = TransactionUpdate::from(tx_update);
            if tx.send(converted).await.is_err() {
                tracing::warn!("Transaction channel closed");
                break;
            }
        }
    }

    Ok(())
}

/// Helper to parse a Geyser ConfirmedTransaction into account keys
pub fn extract_account_keys(tx: &proto::storage::confirmed_block::ConfirmedTransaction) -> Vec<Pubkey> {
    let mut keys = Vec::new();
    
    if let Some(transaction) = &tx.transaction {
        if let Some(message) = &transaction.message {
            for key_bytes in &message.account_keys {
                if key_bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(key_bytes);
                    keys.push(Pubkey::new_from_array(arr));
                }
            }
            
            // Add loaded addresses from ALTs
            if let Some(meta) = &tx.meta {
                for addr in &meta.loaded_writable_addresses {
                    if addr.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(addr);
                        keys.push(Pubkey::new_from_array(arr));
                    }
                }
                for addr in &meta.loaded_readonly_addresses {
                    if addr.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(addr);
                        keys.push(Pubkey::new_from_array(arr));
                    }
                }
            }
        }
    }
    
    keys
}

/// Helper to extract instructions from a Geyser ConfirmedTransaction
pub fn extract_instructions(
    tx: &proto::storage::confirmed_block::ConfirmedTransaction,
) -> Vec<(u32, Vec<u8>, Vec<u8>)> {
    // Returns: Vec<(program_id_index, accounts, data)>
    let mut instructions = Vec::new();
    
    if let Some(transaction) = &tx.transaction {
        if let Some(message) = &transaction.message {
            for ix in &message.instructions {
                instructions.push((
                    ix.program_id_index,
                    ix.accounts.clone(),
                    ix.data.clone(),
                ));
            }
        }
    }
    
    instructions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_geyser_config_default() {
        let config = GeyserConfig::default();
        assert_eq!(config.endpoint, "http://localhost:10000");
    }

    #[test]
    fn test_slot_status_from_i32() {
        assert_eq!(SlotStatus::from(0), SlotStatus::Confirmed);
        assert_eq!(SlotStatus::from(1), SlotStatus::Processed);
        assert_eq!(SlotStatus::from(2), SlotStatus::Rooted);
        assert_eq!(SlotStatus::from(3), SlotStatus::FirstShredReceived);
    }
}
