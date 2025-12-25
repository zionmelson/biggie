//! Turbine Integration
//!
//! Integrates direct Turbine UDP reception with the MEV pipeline,
//! using ShredStream as a fallback.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{info, warn, debug, error};

use solana_sdk::transaction::VersionedTransaction;

use crate::turbine::{
    TurbineReceiver, TurbineConfig, TurbineStatsSnapshot,
    CompleteFecSet, AssembledEntries, run_entry_assembler,
};

// ============================================================================
// Unified Transaction Source
// ============================================================================

/// Source of the transaction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionSource {
    /// Direct Turbine UDP - fastest
    Turbine,
    /// ShredStream proxy - fallback
    ShredStream,
    /// Geyser - ground truth
    Geyser,
}

/// A transaction with its source information
#[derive(Debug, Clone)]
pub struct SourcedTransaction {
    /// The transaction
    pub transaction: VersionedTransaction,
    /// Where this transaction came from
    pub source: TransactionSource,
    /// Slot number
    pub slot: u64,
    /// Time received (for latency tracking)
    pub received_at: Instant,
}

// ============================================================================
// Hybrid Pipeline
// ============================================================================

/// Configuration for the hybrid Turbine + ShredStream pipeline
#[derive(Debug, Clone)]
pub struct HybridPipelineConfig {
    /// Turbine receiver configuration
    pub turbine: TurbineConfig,
    /// Whether to enable Turbine (can be disabled for fallback-only mode)
    pub enable_turbine: bool,
    /// Timeout for considering Turbine unhealthy
    pub turbine_health_timeout: Duration,
    /// Channel buffer size for transactions
    pub tx_channel_size: usize,
}

impl Default for HybridPipelineConfig {
    fn default() -> Self {
        Self {
            turbine: TurbineConfig::default(),
            enable_turbine: true,
            turbine_health_timeout: Duration::from_secs(10),
            tx_channel_size: 10_000,
        }
    }
}

/// Hybrid pipeline that uses Turbine as primary and ShredStream as fallback
pub struct HybridPipeline {
    /// Turbine receiver
    turbine_receiver: Option<TurbineReceiver>,
    /// Configuration
    config: HybridPipelineConfig,
    /// Transaction output channel
    tx_sender: mpsc::Sender<SourcedTransaction>,
    /// Last Turbine activity (for health check)
    last_turbine_activity: Option<Instant>,
    /// Whether Turbine is considered healthy
    turbine_healthy: bool,
}

impl HybridPipeline {
    /// Create a new hybrid pipeline
    pub fn new(config: HybridPipelineConfig) -> (Self, mpsc::Receiver<SourcedTransaction>) {
        let (tx_sender, tx_receiver) = mpsc::channel(config.tx_channel_size);
        
        let pipeline = Self {
            turbine_receiver: None,
            config,
            tx_sender,
            last_turbine_activity: None,
            turbine_healthy: false,
        };
        
        (pipeline, tx_receiver)
    }
    
    /// Start the hybrid pipeline
    pub async fn start(&mut self) -> anyhow::Result<()> {
        if self.config.enable_turbine {
            info!("starting Turbine UDP receiver as primary source");
            
            let mut receiver = TurbineReceiver::new(self.config.turbine.clone());
            let fec_rx = receiver.start().await?;
            
            // Start entry assembler
            let (entries_tx, mut entries_rx) = mpsc::channel::<AssembledEntries>(1000);
            
            // Spawn entry assembler task
            tokio::spawn(async move {
                run_entry_assembler(fec_rx, entries_tx).await;
            });
            
            // Spawn transaction forwarder
            let tx_sender = self.tx_sender.clone();
            tokio::spawn(async move {
                while let Some(assembled) = entries_rx.recv().await {
                    let received_at = Instant::now();
                    let slot = assembled.slot;
                    
                    for tx in assembled.transactions {
                        let sourced = SourcedTransaction {
                            transaction: tx,
                            source: TransactionSource::Turbine,
                            slot,
                            received_at,
                        };
                        
                        if tx_sender.send(sourced).await.is_err() {
                            break;
                        }
                    }
                }
            });
            
            self.turbine_receiver = Some(receiver);
            self.turbine_healthy = true;
            self.last_turbine_activity = Some(Instant::now());
            
            info!("Turbine pipeline started successfully");
        } else {
            info!("Turbine disabled, using ShredStream only");
        }
        
        Ok(())
    }
    
    /// Check Turbine health
    pub fn check_health(&mut self) -> bool {
        if let Some(last_activity) = self.last_turbine_activity {
            if last_activity.elapsed() > self.config.turbine_health_timeout {
                if self.turbine_healthy {
                    warn!(
                        timeout_secs = self.config.turbine_health_timeout.as_secs(),
                        "Turbine marked unhealthy due to inactivity"
                    );
                    self.turbine_healthy = false;
                }
            }
        }
        self.turbine_healthy
    }
    
    /// Update activity timestamp (call when receiving Turbine data)
    pub fn mark_turbine_active(&mut self) {
        self.last_turbine_activity = Some(Instant::now());
        if !self.turbine_healthy {
            info!("Turbine marked healthy again");
            self.turbine_healthy = true;
        }
    }
    
    /// Forward a transaction from ShredStream (fallback source)
    pub async fn forward_shredstream_tx(
        &self,
        tx: VersionedTransaction,
        slot: u64,
    ) -> Result<(), mpsc::error::SendError<SourcedTransaction>> {
        let sourced = SourcedTransaction {
            transaction: tx,
            source: TransactionSource::ShredStream,
            slot,
            received_at: Instant::now(),
        };
        
        self.tx_sender.send(sourced).await
    }
    
    /// Get Turbine statistics if available
    pub fn turbine_stats(&self) -> Option<TurbineStatsSnapshot> {
        self.turbine_receiver.as_ref().map(|r| r.stats())
    }
    
    /// Whether Turbine is the primary source
    pub fn is_turbine_primary(&self) -> bool {
        self.config.enable_turbine && self.turbine_healthy
    }
    
    /// Shutdown the pipeline
    pub async fn shutdown(&mut self) {
        if let Some(mut receiver) = self.turbine_receiver.take() {
            receiver.shutdown().await;
        }
    }
}

// ============================================================================
// Deduplication
// ============================================================================

/// Transaction deduplicator across sources
pub struct TxDeduplicator {
    /// Recent transaction signatures (circular buffer of hashes)
    recent_sigs: Vec<[u8; 64]>,
    /// Write position
    write_pos: usize,
    /// Capacity
    capacity: usize,
}

impl TxDeduplicator {
    pub fn new(capacity: usize) -> Self {
        Self {
            recent_sigs: Vec::with_capacity(capacity),
            write_pos: 0,
            capacity,
        }
    }
    
    /// Check if we've seen this transaction signature
    /// Returns true if this is a NEW transaction (not a duplicate)
    pub fn is_new(&mut self, sig: &[u8; 64]) -> bool {
        // Linear search is fine for small buffers
        // For production, use a proper bloom filter or hash set
        if self.recent_sigs.iter().any(|s| s == sig) {
            return false;
        }
        
        // Add to buffer
        if self.recent_sigs.len() < self.capacity {
            self.recent_sigs.push(*sig);
        } else {
            self.recent_sigs[self.write_pos] = *sig;
            self.write_pos = (self.write_pos + 1) % self.capacity;
        }
        
        true
    }
    
    /// Clear all entries for a new slot
    pub fn clear(&mut self) {
        self.recent_sigs.clear();
        self.write_pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_deduplicator() {
        let mut dedup = TxDeduplicator::new(10);
        
        let sig1 = [1u8; 64];
        let sig2 = [2u8; 64];
        
        assert!(dedup.is_new(&sig1));
        assert!(!dedup.is_new(&sig1)); // Duplicate
        assert!(dedup.is_new(&sig2));
        assert!(!dedup.is_new(&sig2)); // Duplicate
    }
    
    #[test]
    fn test_config_defaults() {
        let config = HybridPipelineConfig::default();
        assert!(config.enable_turbine);
        assert_eq!(config.turbine_health_timeout, Duration::from_secs(10));
    }
}
