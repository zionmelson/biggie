//! Geyser gRPC Client for Local Validator
//!
//! Connects to the local validator's Geyser plugin for ultra-low-latency
//! account and block streaming. This replaces external data sources
//! (CoreCast, shredstream proxy) with direct validator access.
//!
//! Available streams:
//! - SlotUpdates: Slot status (FIRST_SHRED_RECEIVED, PROCESSED, CONFIRMED, ROOTED)
//! - BlockUpdates: Block completion with blockhash
//! - AccountUpdates: Real-time account changes (vaults, CLMM pools)

use anyhow::{Context, Result};
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;
use std::time::Duration;
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
    EmptyRequest, SubscribeSlotUpdateRequest,
    SubscribeBlockUpdatesRequest, SubscribeAccountUpdatesRequest,
    TimestampedSlotUpdate, TimestampedBlockUpdate,
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

/// Geyser client for streaming data from local validator
pub struct GeyserClient {
    config: GeyserConfig,
    channel: Channel,
}

/// Maximum gRPC message size (200 MB) - needed for large account subscription batches
const MAX_GRPC_MESSAGE_SIZE: usize = 200 * 1024 * 1024;

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
        let mut client = GeyserGrpcClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_GRPC_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_GRPC_MESSAGE_SIZE);
        let response = client
            .get_heartbeat_interval(EmptyRequest {})
            .await?
            .into_inner();
        Ok(response.heartbeat_interval_ms)
    }

    /// Subscribe to slot updates
    /// 
    /// Useful for tracking slot timing (FIRST_SHRED_RECEIVED is earliest signal)
    pub async fn subscribe_slots(&self) -> Result<tonic::Streaming<TimestampedSlotUpdate>> {
        let mut client = GeyserGrpcClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_GRPC_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_GRPC_MESSAGE_SIZE);
        let request = tonic::Request::new(SubscribeSlotUpdateRequest {});
        let response = client.subscribe_slot_updates(request).await?;
        Ok(response.into_inner())
    }

    /// Subscribe to block updates
    /// 
    /// Provides blockhash immediately when slot completes
    pub async fn subscribe_blocks(&self) -> Result<tonic::Streaming<TimestampedBlockUpdate>> {
        let mut client = GeyserGrpcClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_GRPC_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_GRPC_MESSAGE_SIZE);
        let request = tonic::Request::new(SubscribeBlockUpdatesRequest {});
        let response = client.subscribe_block_updates(request).await?;
        Ok(response.into_inner())
    }

    /// Subscribe to specific account updates (e.g., vault accounts for reserves)
    /// 
    /// For tracking token vault balances in real-time
    pub async fn subscribe_accounts(
        &self,
        accounts: Vec<Pubkey>,
    ) -> Result<tonic::Streaming<proto::geyser::TimestampedAccountUpdate>> {
        let mut client = GeyserGrpcClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_GRPC_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_GRPC_MESSAGE_SIZE);
        let account_bytes: Vec<Vec<u8>> = accounts.iter().map(|a| a.to_bytes().to_vec()).collect();
        let request = tonic::Request::new(SubscribeAccountUpdatesRequest { accounts: account_bytes });
        let response = client.subscribe_account_updates(request).await?;
        Ok(response.into_inner())
    }
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
