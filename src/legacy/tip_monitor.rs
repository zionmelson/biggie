//! Jito Tip Floor Monitor
//!
//! Monitors the Jito tip floor API to ensure competitive tipping.
//! Tip floors are updated every few seconds and help bots stay competitive
//! in the bundle auction without over-paying.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::constants::{
    DEFAULT_TIP_P50_LAMPORTS, DEFAULT_TIP_P75_LAMPORTS, DEFAULT_TIP_P95_LAMPORTS,
    DEFAULT_TIP_P99_LAMPORTS, TIP_FLOOR_STALE_SECS, TIP_FLOOR_POLL_SECS,
};

/// Default tip floor endpoint
pub const DEFAULT_TIP_FLOOR_URL: &str = "https://bundles.jito.wtf/api/v1/bundles/tip_floor";

/// Default tip stream WebSocket endpoint
pub const DEFAULT_TIP_STREAM_URL: &str = "wss://bundles.jito.wtf/api/v1/bundles/tip_stream";

/// Tip floor response from Jito API (in SOL)
#[derive(Debug, Clone, Deserialize)]
pub struct TipFloorResponse {
    pub time: String,
    /// 25th percentile of landed tips (SOL)
    pub landed_tips_25th_percentile: f64,
    /// 50th percentile of landed tips (SOL)
    pub landed_tips_50th_percentile: f64,
    /// 75th percentile of landed tips (SOL)
    pub landed_tips_75th_percentile: f64,
    /// 95th percentile of landed tips (SOL)
    pub landed_tips_95th_percentile: f64,
    /// 99th percentile of landed tips (SOL)
    pub landed_tips_99th_percentile: f64,
    /// EMA of 50th percentile (SOL)
    pub ema_landed_tips_50th_percentile: f64,
}

impl TipFloorResponse {
    /// Convert 50th percentile to lamports
    pub fn p50_lamports(&self) -> u64 {
        (self.landed_tips_50th_percentile * 1_000_000_000.0) as u64
    }
    
    /// Convert 75th percentile to lamports
    pub fn p75_lamports(&self) -> u64 {
        (self.landed_tips_75th_percentile * 1_000_000_000.0) as u64
    }
    
    /// Convert 95th percentile to lamports
    pub fn p95_lamports(&self) -> u64 {
        (self.landed_tips_95th_percentile * 1_000_000_000.0) as u64
    }
    
    /// Convert 99th percentile to lamports
    pub fn p99_lamports(&self) -> u64 {
        (self.landed_tips_99th_percentile * 1_000_000_000.0) as u64
    }
    
    /// Convert EMA to lamports
    pub fn ema_lamports(&self) -> u64 {
        (self.ema_landed_tips_50th_percentile * 1_000_000_000.0) as u64
    }
}

/// Tip floor cache with atomic access
pub struct TipFloorCache {
    /// P50 tip floor in lamports
    p50_lamports: AtomicU64,
    /// P75 tip floor in lamports
    p75_lamports: AtomicU64,
    /// P95 tip floor in lamports
    p95_lamports: AtomicU64,
    /// P99 tip floor in lamports
    p99_lamports: AtomicU64,
    /// EMA tip floor in lamports
    ema_lamports: AtomicU64,
    /// Last update timestamp
    last_update: RwLock<Instant>,
    /// Update count for stats
    update_count: AtomicU64,
}

impl Default for TipFloorCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TipFloorCache {
    pub fn new() -> Self {
        // Initialize with sensible defaults from constants
        Self {
            p50_lamports: AtomicU64::new(DEFAULT_TIP_P50_LAMPORTS),
            p75_lamports: AtomicU64::new(DEFAULT_TIP_P75_LAMPORTS),
            p95_lamports: AtomicU64::new(DEFAULT_TIP_P95_LAMPORTS),
            p99_lamports: AtomicU64::new(DEFAULT_TIP_P99_LAMPORTS),
            ema_lamports: AtomicU64::new(DEFAULT_TIP_P75_LAMPORTS), // Use P75 as EMA default
            last_update: RwLock::new(Instant::now()),
            update_count: AtomicU64::new(0),
        }
    }
    
    /// Update the cache with new tip floor data
    pub async fn update(&self, response: &TipFloorResponse) {
        self.p50_lamports.store(response.p50_lamports(), Ordering::Relaxed);
        self.p75_lamports.store(response.p75_lamports(), Ordering::Relaxed);
        self.p95_lamports.store(response.p95_lamports(), Ordering::Relaxed);
        self.p99_lamports.store(response.p99_lamports(), Ordering::Relaxed);
        self.ema_lamports.store(response.ema_lamports(), Ordering::Relaxed);
        *self.last_update.write().await = Instant::now();
        self.update_count.fetch_add(1, Ordering::Relaxed);
    }
    
    /// Get the 50th percentile tip floor (most common)
    pub fn get_p50(&self) -> u64 {
        self.p50_lamports.load(Ordering::Relaxed)
    }
    
    /// Get the 75th percentile tip floor (competitive)
    pub fn get_p75(&self) -> u64 {
        self.p75_lamports.load(Ordering::Relaxed)
    }
    
    /// Get the 95th percentile tip floor (highly competitive)
    pub fn get_p95(&self) -> u64 {
        self.p95_lamports.load(Ordering::Relaxed)
    }
    
    /// Get the 99th percentile tip floor (maximum priority)
    pub fn get_p99(&self) -> u64 {
        self.p99_lamports.load(Ordering::Relaxed)
    }
    
    /// Get EMA of tip floor (smoothed average)
    pub fn get_ema(&self) -> u64 {
        self.ema_lamports.load(Ordering::Relaxed)
    }
    
    /// Check if cache data is stale (older than configured threshold)
    pub async fn is_stale(&self) -> bool {
        self.last_update.read().await.elapsed() > Duration::from_secs(TIP_FLOOR_STALE_SECS)
    }
    
    /// Get update count for monitoring
    pub fn update_count(&self) -> u64 {
        self.update_count.load(Ordering::Relaxed)
    }
}

/// Tip floor monitor that periodically fetches tip data from Jito
pub struct TipFloorMonitor {
    cache: Arc<TipFloorCache>,
    endpoint: String,
    poll_interval: Duration,
}

impl TipFloorMonitor {
    pub fn new(endpoint: Option<String>) -> Self {
        Self {
            cache: Arc::new(TipFloorCache::new()),
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_TIP_FLOOR_URL.to_string()),
            poll_interval: Duration::from_secs(TIP_FLOOR_POLL_SECS),
        }
    }
    
    /// Get the shared cache
    pub fn cache(&self) -> Arc<TipFloorCache> {
        Arc::clone(&self.cache)
    }
    
    /// Fetch tip floor data once
    async fn fetch_tip_floor(&self) -> Result<TipFloorResponse> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
            
        let response: Vec<TipFloorResponse> = client
            .get(&self.endpoint)
            .send()
            .await
            .context("Failed to fetch tip floor")?
            .json()
            .await
            .context("Failed to parse tip floor response")?;
            
        response.into_iter().next().context("Empty tip floor response")
    }
    
    /// Start the monitoring loop (runs until cancelled)
    pub async fn run(&self) -> Result<()> {
        tracing::info!("Starting tip floor monitor (endpoint: {})", self.endpoint);
        
        loop {
            match self.fetch_tip_floor().await {
                Ok(response) => {
                    self.cache.update(&response).await;
                    tracing::debug!(
                        "Tip floor updated: p50={} p75={} p95={} p99={} ema={} lamports",
                        response.p50_lamports(),
                        response.p75_lamports(),
                        response.p95_lamports(),
                        response.p99_lamports(),
                        response.ema_lamports(),
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch tip floor: {}", e);
                }
            }
            
            tokio::time::sleep(self.poll_interval).await;
        }
    }
    
    /// Spawn the monitor as a background task
    pub fn spawn(self) -> (tokio::task::JoinHandle<()>, Arc<TipFloorCache>) {
        let cache = Arc::clone(&self.cache);
        let handle = tokio::spawn(async move {
            if let Err(e) = self.run().await {
                tracing::error!("Tip floor monitor error: {}", e);
            }
        });
        (handle, cache)
    }
}

/// Dynamic tip calculator that uses tip floor data
pub struct DynamicTipCalculator {
    cache: Arc<TipFloorCache>,
    /// Base multiplier over P75 floor (e.g., 1.5 = 50% above floor)
    floor_multiplier: f64,
    /// Percentage of profit to tip (in bps, e.g., 5000 = 50%)
    profit_share_bps: u64,
    /// Maximum tip (cap for safety)
    max_tip_lamports: u64,
    /// Minimum tip (Jito requires 1000 lamports)
    min_tip_lamports: u64,
}

impl DynamicTipCalculator {
    pub fn new(cache: Arc<TipFloorCache>) -> Self {
        Self {
            cache,
            floor_multiplier: 1.5,  // 50% above P75 floor
            profit_share_bps: 4000, // 40% of profit - keep 60% for ourselves
            max_tip_lamports: 10_000_000_000, // 10 SOL max
            min_tip_lamports: 1_000, // Jito minimum
        }
    }
    
    /// Create with custom settings
    pub fn with_config(
        cache: Arc<TipFloorCache>,
        floor_multiplier: f64,
        profit_share_bps: u64,
        max_tip_lamports: u64,
    ) -> Self {
        Self {
            cache,
            floor_multiplier,
            profit_share_bps,
            max_tip_lamports,
            min_tip_lamports: 1_000,
        }
    }
    
    /// Calculate optimal tip for an opportunity
    /// 
    /// Strategy:
    /// 1. Start with floor (P75 * multiplier) - ensures we're competitive
    /// 2. For high-value opportunities, use profit share
    /// 3. Cap at max_tip for safety
    /// 
    /// The tip should be high enough to win the auction but not waste profit.
    pub fn calculate_tip(&self, expected_profit_lamports: u64) -> u64 {
        // Get current floor (P75 is competitive, P95 for priority)
        let floor = self.cache.get_p75();
        let competitive_floor = ((floor as f64) * self.floor_multiplier) as u64;
        
        // Calculate profit-based tip
        let profit_tip = (expected_profit_lamports as u128 * self.profit_share_bps as u128 / 10000) as u64;
        
        // Use the higher of floor or profit-based tip
        // This ensures we're always competitive with the floor
        let tip = competitive_floor.max(profit_tip);
        
        // Apply bounds
        tip.min(self.max_tip_lamports)
           .max(self.min_tip_lamports)
    }
    
    /// Calculate tip for high-priority opportunities (uses P95 floor)
    pub fn calculate_priority_tip(&self, expected_profit_lamports: u64) -> u64 {
        let floor = self.cache.get_p95();
        let competitive_floor = ((floor as f64) * self.floor_multiplier) as u64;
        let profit_tip = (expected_profit_lamports as u128 * self.profit_share_bps as u128 / 10000) as u64;
        
        competitive_floor.max(profit_tip)
            .min(self.max_tip_lamports)
            .max(self.min_tip_lamports)
    }
    
    /// Get stats for logging
    pub fn stats_summary(&self) -> String {
        format!(
            "tip_floor: p50={} p75={} p95={} ema={} lamports",
            self.cache.get_p50(),
            self.cache.get_p75(),
            self.cache.get_p95(),
            self.cache.get_ema(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_tip_floor_response_conversion() {
        let response = TipFloorResponse {
            time: "2025-01-01T00:00:00Z".to_string(),
            landed_tips_25th_percentile: 0.000001,
            landed_tips_50th_percentile: 0.000001,
            landed_tips_75th_percentile: 0.0000025,
            landed_tips_95th_percentile: 0.000005,
            landed_tips_99th_percentile: 0.00015,
            ema_landed_tips_50th_percentile: 0.0000024,
        };
        
        assert_eq!(response.p50_lamports(), 1000);
        assert_eq!(response.p75_lamports(), 2500);
        assert_eq!(response.p95_lamports(), 5000);
        assert_eq!(response.p99_lamports(), 150000);
    }
    
    #[tokio::test]
    async fn test_dynamic_tip_calculator() {
        let cache = Arc::new(TipFloorCache::new());
        // Update with test values
        let response = TipFloorResponse {
            time: "2025-01-01T00:00:00Z".to_string(),
            landed_tips_25th_percentile: 0.000001,
            landed_tips_50th_percentile: 0.000001,
            landed_tips_75th_percentile: 0.000010, // 10k lamports
            landed_tips_95th_percentile: 0.000050, // 50k lamports
            landed_tips_99th_percentile: 0.00015,
            ema_landed_tips_50th_percentile: 0.0000024,
        };
        cache.update(&response).await;
        
        let calc = DynamicTipCalculator::new(cache);
        
        // Small profit should use floor-based tip
        let small_tip = calc.calculate_tip(100_000); // 0.0001 SOL profit
        assert!(small_tip >= 10_000, "Should use floor for small profit");
        
        // Large profit should use profit-based tip (40% default)
        let large_tip = calc.calculate_tip(10_000_000_000); // 10 SOL profit
        assert!(large_tip >= 4_000_000_000, "Should use 40% of profit");
    }
}
