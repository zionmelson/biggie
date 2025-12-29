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
#[allow(unused_imports)] // SlotUpdateStatus used only in unified_pipeline
use proto::geyser::{
    EmptyRequest, SubscribeSlotUpdateRequest, SubscribeTransactionUpdatesRequest,
    SubscribeBlockUpdatesRequest, SubscribeProgramsUpdatesRequest, SubscribeAccountUpdatesRequest,
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

    /// Subscribe to transaction updates (non-vote transactions)
    /// 
    /// This is the primary stream for arb - gives us full transaction data
    /// as transactions are processed by the validator.
    pub async fn subscribe_transactions(
        &self,
    ) -> Result<tonic::Streaming<TimestampedTransactionUpdate>> {
        let mut client = GeyserGrpcClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_GRPC_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_GRPC_MESSAGE_SIZE);
        let request = tonic::Request::new(SubscribeTransactionUpdatesRequest {});
        let response = client.subscribe_transaction_updates(request).await?;
        Ok(response.into_inner())
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

    /// Subscribe to program account updates
    /// 
    /// For tracking DEX pool state changes in real-time
    pub async fn subscribe_programs(
        &self,
        program_ids: Vec<Pubkey>,
    ) -> Result<tonic::Streaming<proto::geyser::TimestampedAccountUpdate>> {
        let mut client = GeyserGrpcClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_GRPC_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_GRPC_MESSAGE_SIZE);
        let programs: Vec<Vec<u8>> = program_ids.iter().map(|p| p.to_bytes().to_vec()).collect();
        let request = tonic::Request::new(SubscribeProgramsUpdatesRequest { programs });
        let response = client.subscribe_program_updates(request).await?;
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

/// Convert Geyser's ConfirmedTransaction to VersionedTransaction
/// 
/// This is the main conversion function for processing Geyser transaction data.
/// Returns both the transaction and any loaded addresses from ALTs.
pub fn convert_geyser_transaction(
    confirmed_tx: &proto::storage::confirmed_block::ConfirmedTransaction,
) -> Option<(solana_sdk::transaction::VersionedTransaction, Option<solana_message::v0::LoadedAddresses>)> {
    use solana_sdk::signature::Signature;
    use solana_sdk::message::{Message, VersionedMessage, v0};
    use solana_message::compiled_instruction::CompiledInstruction;
    use solana_message::v0::LoadedAddresses;

    let transaction = confirmed_tx.transaction.as_ref()?;
    let message = transaction.message.as_ref()?;

    // Parse signatures
    let signatures: Vec<Signature> = transaction
        .signatures
        .iter()
        .filter_map(|sig_bytes| {
            if sig_bytes.len() == 64 {
                let mut arr = [0u8; 64];
                arr.copy_from_slice(sig_bytes);
                Some(Signature::from(arr))
            } else {
                None
            }
        })
        .collect();

    if signatures.is_empty() {
        return None;
    }

    // Parse account keys
    let account_keys: Vec<Pubkey> = message
        .account_keys
        .iter()
        .filter_map(|key_bytes| {
            if key_bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(key_bytes);
                Some(Pubkey::new_from_array(arr))
            } else {
                None
            }
        })
        .collect();

    // Parse recent blockhash
    if message.recent_blockhash.len() != 32 {
        return None;
    }
    let mut blockhash_arr = [0u8; 32];
    blockhash_arr.copy_from_slice(&message.recent_blockhash);
    let recent_blockhash = Hash::new_from_array(blockhash_arr);

    // Parse instructions
    let instructions: Vec<CompiledInstruction> = message
        .instructions
        .iter()
        .map(|ix| CompiledInstruction {
            program_id_index: ix.program_id_index as u8,
            accounts: ix.accounts.clone(),
            data: ix.data.clone(),
        })
        .collect();

    // Check if this is a v0 message (has address table lookups)
    let has_lookups = !message.address_table_lookups.is_empty();

    // Parse loaded addresses from meta
    let loaded_addresses = if let Some(meta) = &confirmed_tx.meta {
        let writable: Vec<Pubkey> = meta
            .loaded_writable_addresses
            .iter()
            .filter_map(|b| {
                if b.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(b);
                    Some(Pubkey::new_from_array(arr))
                } else {
                    None
                }
            })
            .collect();

        let readonly: Vec<Pubkey> = meta
            .loaded_readonly_addresses
            .iter()
            .filter_map(|b| {
                if b.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(b);
                    Some(Pubkey::new_from_array(arr))
                } else {
                    None
                }
            })
            .collect();

        if writable.is_empty() && readonly.is_empty() {
            None
        } else {
            Some(LoadedAddresses { writable, readonly })
        }
    } else {
        None
    };

    // Build the VersionedMessage
    let versioned_message = if has_lookups {
        // V0 message with address lookups
        let address_table_lookups: Vec<solana_sdk::message::v0::MessageAddressTableLookup> =
            message
                .address_table_lookups
                .iter()
                .filter_map(|lookup| {
                    if lookup.account_key.len() != 32 {
                        return None;
                    }
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&lookup.account_key);
                    Some(solana_sdk::message::v0::MessageAddressTableLookup {
                        account_key: Pubkey::new_from_array(arr),
                        writable_indexes: lookup.writable_indexes.clone(),
                        readonly_indexes: lookup.readonly_indexes.clone(),
                    })
                })
                .collect();

        // For v0 messages, we need to construct the header
        let header = message.header.as_ref().map(|h| solana_sdk::message::MessageHeader {
            num_required_signatures: h.num_required_signatures as u8,
            num_readonly_signed_accounts: h.num_readonly_signed_accounts as u8,
            num_readonly_unsigned_accounts: h.num_readonly_unsigned_accounts as u8,
        }).unwrap_or_default();

        let v0_message = v0::Message {
            header,
            account_keys,
            recent_blockhash,
            instructions,
            address_table_lookups,
        };

        VersionedMessage::V0(v0_message)
    } else {
        // Legacy message
        let header = message.header.as_ref().map(|h| solana_sdk::message::MessageHeader {
            num_required_signatures: h.num_required_signatures as u8,
            num_readonly_signed_accounts: h.num_readonly_signed_accounts as u8,
            num_readonly_unsigned_accounts: h.num_readonly_unsigned_accounts as u8,
        }).unwrap_or_default();

        let legacy_message = Message {
            header,
            account_keys,
            recent_blockhash,
            instructions,
        };

        VersionedMessage::Legacy(legacy_message)
    };

    Some((
        solana_sdk::transaction::VersionedTransaction {
            signatures,
            message: versioned_message,
        },
        loaded_addresses,
    ))
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
