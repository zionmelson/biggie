//! UDP Turbine Receiver
//!
//! Receives shreds directly from the local validator's Turbine broadcast.
//! This bypasses the ShredStream proxy for minimum latency.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::io;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn, trace};

use solana_sdk::clock::Slot;

use crate::turbine::shred::{
    parse_shred, peek_shred_header, ParsedShred, ShredType,
    SHRED_SIZE,
};
use crate::turbine::fec::{
    FecSetAssembler, FecAssemblerConfig, CompleteFecSet,
};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the Turbine receiver
#[derive(Debug, Clone)]
pub struct TurbineConfig {
    /// UDP address to bind for receiving shreds (e.g., "0.0.0.0:8002")
    pub bind_addr: SocketAddr,
    
    /// Validator's TVU address (where shreds come from)
    /// Used for verification, not for binding
    pub validator_tvu_addr: Option<SocketAddr>,
    
    /// Size of the UDP receive buffer
    pub recv_buffer_size: usize,
    
    /// Number of receiver threads
    pub num_receiver_threads: usize,
    
    /// FEC assembler configuration
    pub fec_config: FecAssemblerConfig,
    
    /// Minimum slot to process (ignore older shreds)
    pub min_slot: Option<Slot>,
    
    /// Enable packet deduplication
    pub enable_dedup: bool,
    
    /// Channel buffer size for shreds
    pub shred_channel_size: usize,
    
    /// Channel buffer size for completed FEC sets
    pub fec_channel_size: usize,
}

impl Default for TurbineConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:8002".parse().unwrap(),
            validator_tvu_addr: None,
            recv_buffer_size: 128 * 1024 * 1024, // 128MB receive buffer
            num_receiver_threads: 2,
            fec_config: FecAssemblerConfig::default(),
            min_slot: None,
            enable_dedup: true,
            shred_channel_size: 10_000,
            fec_channel_size: 1_000,
        }
    }
}

// ============================================================================
// Statistics
// ============================================================================

#[derive(Debug, Default)]
pub struct TurbineStats {
    /// Total packets received
    pub packets_received: AtomicU64,
    /// Packets that failed to parse
    pub packets_failed: AtomicU64,
    /// Data shreds received
    pub data_shreds: AtomicU64,
    /// Coding shreds received
    pub coding_shreds: AtomicU64,
    /// Packets dropped due to channel full
    pub channel_drops: AtomicU64,
    /// Packets filtered by slot
    pub filtered_by_slot: AtomicU64,
    /// Duplicate packets filtered
    pub duplicates_filtered: AtomicU64,
    /// Total bytes received
    pub bytes_received: AtomicU64,
    /// Most recent slot seen
    pub current_slot: AtomicU64,
}

impl TurbineStats {
    pub fn snapshot(&self) -> TurbineStatsSnapshot {
        TurbineStatsSnapshot {
            packets_received: self.packets_received.load(Ordering::Relaxed),
            packets_failed: self.packets_failed.load(Ordering::Relaxed),
            data_shreds: self.data_shreds.load(Ordering::Relaxed),
            coding_shreds: self.coding_shreds.load(Ordering::Relaxed),
            channel_drops: self.channel_drops.load(Ordering::Relaxed),
            filtered_by_slot: self.filtered_by_slot.load(Ordering::Relaxed),
            duplicates_filtered: self.duplicates_filtered.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            current_slot: self.current_slot.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurbineStatsSnapshot {
    pub packets_received: u64,
    pub packets_failed: u64,
    pub data_shreds: u64,
    pub coding_shreds: u64,
    pub channel_drops: u64,
    pub filtered_by_slot: u64,
    pub duplicates_filtered: u64,
    pub bytes_received: u64,
    pub current_slot: u64,
}

// ============================================================================
// Turbine Receiver
// ============================================================================

/// Main Turbine receiver that handles UDP shred reception
pub struct TurbineReceiver {
    /// Configuration
    config: TurbineConfig,
    /// Statistics
    stats: Arc<TurbineStats>,
    /// Shutdown flag
    shutdown: Arc<AtomicBool>,
    /// Receiver task handles
    handles: Vec<JoinHandle<()>>,
}

impl TurbineReceiver {
    /// Create a new Turbine receiver
    pub fn new(config: TurbineConfig) -> Self {
        Self {
            config,
            stats: Arc::new(TurbineStats::default()),
            shutdown: Arc::new(AtomicBool::new(false)),
            handles: Vec::new(),
        }
    }
    
    /// Start the receiver and return a channel of completed FEC sets
    pub async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<CompleteFecSet>> {
        info!(
            bind_addr = %self.config.bind_addr,
            recv_buffer = self.config.recv_buffer_size,
            threads = self.config.num_receiver_threads,
            "starting Turbine UDP receiver"
        );
        
        // Create channels
        let (shred_tx, shred_rx) = mpsc::channel(self.config.shred_channel_size);
        let (fec_tx, fec_rx) = mpsc::channel(self.config.fec_channel_size);
        
        // Start UDP receiver threads
        for thread_id in 0..self.config.num_receiver_threads {
            let socket = self.create_socket()?;
            let shred_tx = shred_tx.clone();
            let stats = self.stats.clone();
            let shutdown = self.shutdown.clone();
            let min_slot = self.config.min_slot;
            
            let handle = tokio::spawn(async move {
                udp_receiver_loop(
                    thread_id,
                    socket,
                    shred_tx,
                    stats,
                    shutdown,
                    min_slot,
                ).await;
            });
            
            self.handles.push(handle);
        }
        
        // Start FEC assembler task
        let stats = self.stats.clone();
        let shutdown = self.shutdown.clone();
        let fec_config = self.config.fec_config.clone();
        
        let handle = tokio::spawn(async move {
            fec_assembler_loop(shred_rx, fec_tx, fec_config, stats, shutdown).await;
        });
        self.handles.push(handle);
        
        Ok(fec_rx)
    }
    
    /// Create and configure a UDP socket
    fn create_socket(&self) -> io::Result<UdpSocket> {
        let socket = UdpSocket::bind(self.config.bind_addr)?;
        
        // Set large receive buffer
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        
        // Try to set receive buffer size (may require system tuning)
        // On Linux: sysctl -w net.core.rmem_max=134217728
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = socket.as_raw_fd();
            let buf_size = self.config.recv_buffer_size as libc::c_int;
            
            unsafe {
                let res = libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    &buf_size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
                
                if res != 0 {
                    warn!(
                        requested = self.config.recv_buffer_size,
                        "failed to set UDP receive buffer size"
                    );
                }
            }
        }
        
        // Enable non-blocking for tokio
        socket.set_nonblocking(true)?;
        
        Ok(socket)
    }
    
    /// Get current statistics
    pub fn stats(&self) -> TurbineStatsSnapshot {
        self.stats.snapshot()
    }
    
    /// Update minimum slot filter
    /// Note: This would require additional synchronization in production
    /// For now, min_slot is set at startup
    #[allow(unused_variables)]
    pub fn set_min_slot(&self, slot: Slot) {
        // TODO: Implement atomic min_slot update
    }
    
    /// Shutdown the receiver
    pub async fn shutdown(&mut self) {
        info!("shutting down Turbine receiver");
        self.shutdown.store(true, Ordering::SeqCst);
        
        for handle in self.handles.drain(..) {
            let _ = handle.await;
        }
    }
}

// ============================================================================
// Receiver Loop
// ============================================================================

/// UDP receiver loop running on a dedicated thread
async fn udp_receiver_loop(
    thread_id: usize,
    socket: UdpSocket,
    shred_tx: mpsc::Sender<ParsedShred>,
    stats: Arc<TurbineStats>,
    shutdown: Arc<AtomicBool>,
    min_slot: Option<Slot>,
) {
    info!(thread_id, "UDP receiver thread started");
    
    // Use tokio's async UDP
    let socket = tokio::net::UdpSocket::from_std(socket).expect("failed to convert socket");
    
    // Pre-allocate receive buffer (slightly larger than max shred size)
    let mut buf = vec![0u8; SHRED_SIZE + 256];
    
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        
        // Receive packet
        match socket.recv_from(&mut buf).await {
            Ok((len, _src)) => {
                stats.packets_received.fetch_add(1, Ordering::Relaxed);
                stats.bytes_received.fetch_add(len as u64, Ordering::Relaxed);
                
                // Quick header peek for filtering
                if let Some((shred_type, slot, _index, _fec_set)) = peek_shred_header(&buf[..len]) {
                    // Filter old slots
                    if let Some(min) = min_slot {
                        if slot < min {
                            stats.filtered_by_slot.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    }
                    
                    // Update current slot
                    stats.current_slot.fetch_max(slot, Ordering::Relaxed);
                    
                    // Update type-specific stats
                    match shred_type {
                        ShredType::Data => stats.data_shreds.fetch_add(1, Ordering::Relaxed),
                        ShredType::Code => stats.coding_shreds.fetch_add(1, Ordering::Relaxed),
                    };
                    
                    // Parse full shred
                    match parse_shred(&buf[..len]) {
                        Ok(shred) => {
                            // Send to assembler
                            match shred_tx.try_send(shred) {
                                Ok(_) => {}
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    stats.channel_drops.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    warn!("shred channel closed");
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            stats.packets_failed.fetch_add(1, Ordering::Relaxed);
                            trace!(thread_id, error = %e, "failed to parse shred");
                        }
                    }
                } else {
                    stats.packets_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Timeout, continue
                tokio::time::sleep(Duration::from_micros(100)).await;
            }
            Err(e) => {
                error!(thread_id, error = %e, "UDP receive error");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
    
    info!(thread_id, "UDP receiver thread exiting");
}

/// FEC assembler loop
async fn fec_assembler_loop(
    mut shred_rx: mpsc::Receiver<ParsedShred>,
    fec_tx: mpsc::Sender<CompleteFecSet>,
    config: FecAssemblerConfig,
    _stats: Arc<TurbineStats>,
    shutdown: Arc<AtomicBool>,
) {
    info!("FEC assembler started");
    
    let mut assembler = FecSetAssembler::new(fec_tx, config);
    let mut last_stats_log = Instant::now();
    
    while let Some(shred) = shred_rx.recv().await {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        
        assembler.process_shred(shred).await;
        
        // Log stats periodically
        if last_stats_log.elapsed() > Duration::from_secs(30) {
            let fec_stats = assembler.stats();
            debug!(
                shreds_received = fec_stats.shreds_received,
                data_shreds = fec_stats.data_shreds_received,
                coding_shreds = fec_stats.coding_shreds_received,
                fec_completed = fec_stats.fec_sets_completed,
                fec_expired = fec_stats.fec_sets_expired,
                tracked = assembler.tracked_fec_sets(),
                "FEC assembler stats"
            );
            last_stats_log = Instant::now();
        }
    }
    
    info!("FEC assembler exiting");
}

// ============================================================================
// High-Performance Batch Receiver (Alternative Implementation)
// ============================================================================

/// Batch receiver using recvmmsg for higher throughput
/// Only available on Linux
#[cfg(target_os = "linux")]
pub mod batch {
    use super::*;
    
    /// Number of packets to receive in a single syscall
    const BATCH_SIZE: usize = 64;
    
    /// High-performance batch receiver using recvmmsg
    pub async fn batch_receiver_loop(
        thread_id: usize,
        socket: std::os::unix::io::RawFd,
        shred_tx: mpsc::Sender<ParsedShred>,
        stats: Arc<TurbineStats>,
        shutdown: Arc<AtomicBool>,
        min_slot: Option<Slot>,
    ) {
        // This would use recvmmsg to receive multiple packets per syscall
        // For now, falls back to regular receive
        // TODO: Implement actual recvmmsg batching
        
        unimplemented!("batch recvmmsg receiver");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_config_defaults() {
        let config = TurbineConfig::default();
        assert_eq!(config.bind_addr.port(), 8002);
        assert_eq!(config.num_receiver_threads, 2);
    }
    
    #[test]
    fn test_stats_snapshot() {
        let stats = TurbineStats::default();
        stats.packets_received.store(100, Ordering::Relaxed);
        stats.data_shreds.store(50, Ordering::Relaxed);
        
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.packets_received, 100);
        assert_eq!(snapshot.data_shreds, 50);
    }
}
