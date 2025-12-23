//! Core types for shredstream integration

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use anyhow::{Result, Context};
use bincode::Options;
use solana_entry::entry::Entry as SolanaEntry;
use solana_sdk::transaction::VersionedTransaction;
use tonic::transport::Channel;

// Import generated protobuf types
use crate::jito::shredstream::{
    shredstream_proxy_client::ShredstreamProxyClient,
    SubscribeEntriesRequest,
};

use super::parser::{parse_transaction, TransactionSwaps};

/// Configuration for shredstream client
#[derive(Clone, Debug)]
pub struct ShredstreamConfig {
    pub endpoint: String,
}

/// A decoded transaction from shredstream
#[derive(Debug, Clone)]
pub struct ShredTransaction {
    pub slot: u64,
    pub signature: String,
    pub accounts: Vec<String>,
    pub data: Vec<u8>,
}

/// Statistics for the shredstream monitor
#[derive(Debug, Default)]
pub struct ShredstreamStats {
    pub entries_received: AtomicU64,
    pub transactions_parsed: AtomicU64,
    pub swaps_detected: AtomicU64,
    pub last_slot: AtomicU64,
}

/// Monitor for receiving and processing shredstream data
pub struct ShredstreamMonitor {
    config: ShredstreamConfig,
    stats: Arc<ShredstreamStats>,
}

impl ShredstreamMonitor {
    pub fn new(config: ShredstreamConfig) -> Self {
        Self {
            config,
            stats: Arc::new(ShredstreamStats::default()),
        }
    }

    pub fn stats(&self) -> Arc<ShredstreamStats> {
        Arc::clone(&self.stats)
    }

    /// Run the shredstream monitor, sending parsed swaps to the channel
    pub async fn run(&self, tx: mpsc::Sender<TransactionSwaps>) -> Result<()> {
        tracing::info!("Shredstream connecting to {}", self.config.endpoint);

        // Connect to the shredstream proxy gRPC endpoint
        let channel = Channel::from_shared(self.config.endpoint.clone())
            .context("Invalid endpoint URL")?
            .connect()
            .await
            .context("Failed to connect to shredstream proxy")?;

        let mut client = ShredstreamProxyClient::new(channel);

        // Subscribe to entries stream
        let request = tonic::Request::new(SubscribeEntriesRequest {});
        let mut stream = client
            .subscribe_entries(request)
            .await
            .context("Failed to subscribe to entries")?
            .into_inner();

        tracing::info!("Shredstream subscription established");

        // Bincode options matching solana's serialization
        let bincode_opts = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .allow_trailing_bytes();

        while let Some(entry_msg) = stream.message().await? {
            let slot = entry_msg.slot;
            self.stats.last_slot.store(slot, Ordering::Relaxed);
            self.stats.entries_received.fetch_add(1, Ordering::Relaxed);

            // Deserialize the entries
            let entries: Vec<SolanaEntry> = match bincode_opts.deserialize(&entry_msg.entries) {
                Ok(e) => e,
                Err(e) => {
                    tracing::trace!("Failed to deserialize entries: {}", e);
                    continue;
                }
            };

            let received_at = Instant::now();

            // Process each entry's transactions
            for entry in entries {
                for transaction in entry.transactions {
                    self.stats.transactions_parsed.fetch_add(1, Ordering::Relaxed);

                    // Convert to VersionedTransaction if needed
                    let versioned_tx: VersionedTransaction = transaction.into();

                    // Parse for swaps
                    if let Some(tx_swaps) = parse_transaction(&versioned_tx, slot, received_at, None) {
                        self.stats.swaps_detected.fetch_add(tx_swaps.swaps.len() as u64, Ordering::Relaxed);

                        // Send to channel (non-blocking)
                        if tx.try_send(tx_swaps).is_err() {
                            tracing::trace!("Swap channel full, dropping transaction");
                        }
                    }
                }
            }
        }

        tracing::warn!("Shredstream stream ended");
        Ok(())
    }

    /// Log current statistics
    pub fn log_stats(&self) {
        let entries = self.stats.entries_received.load(Ordering::Relaxed);
        let txs = self.stats.transactions_parsed.load(Ordering::Relaxed);
        let swaps = self.stats.swaps_detected.load(Ordering::Relaxed);
        let slot = self.stats.last_slot.load(Ordering::Relaxed);

        tracing::info!(
            "Shredstream stats: entries={} txs={} swaps={} last_slot={}",
            entries, txs, swaps, slot
        );
    }
}

