//! Multi-Region Parallel Bundle Submission
//!
//! Submits bundles to multiple Jito block engines simultaneously.
//! First arrival wins the auction - this maximizes our chances against
//! searchers who may be colocated with different regions.
//!
//! Supported regions:
//! - NY (New York) - Primary for US East
//! - Amsterdam - Primary for Europe
//! - Frankfurt - Secondary for Europe
//! - Tokyo - Primary for Asia
//!
//! With NY infrastructure, we have ~1ms to NY, ~70ms to AMS, ~100ms to Tokyo.
//! Parallel submission ensures we compete globally.

use crate::jito::{JitoClient, JitoConfig};
use anyhow::{Result, Context};
use solana_sdk::signature::Keypair;
use solana_sdk::transaction::VersionedTransaction;
use std::sync::Arc;
use std::time::Instant;

/// Jito block engine endpoints by region
pub const JITO_ENDPOINTS: &[(&str, &str)] = &[
    ("ny", "https://ny.mainnet.block-engine.jito.wtf"),
    ("amsterdam", "https://amsterdam.mainnet.block-engine.jito.wtf"),
    ("frankfurt", "https://frankfurt.mainnet.block-engine.jito.wtf"),
    ("tokyo", "https://tokyo.mainnet.block-engine.jito.wtf"),
];

/// Result of a multi-region bundle submission
#[derive(Debug, Clone)]
pub struct MultiRegionSubmitResult {
    /// Bundle ID (from first successful submission)
    pub bundle_id: String,
    /// Region that responded first
    pub first_region: String,
    /// Time to first response
    pub first_response_ms: u64,
    /// Results from all regions
    pub region_results: Vec<RegionResult>,
}

#[derive(Debug, Clone)]
pub struct RegionResult {
    pub region: String,
    pub result: Result<String, String>,
    pub latency_ms: u64,
}

/// Multi-region Jito client pool
pub struct MultiRegionJito {
    clients: Vec<(String, Arc<JitoClient>)>,
    /// Optional CA PEM for custom TLS
    ca_pem: Option<String>,
}

impl MultiRegionJito {
    /// Create clients for specified endpoints
    pub async fn new(
        endpoints: &[(String, String)], // (region_name, endpoint_url)
        auth_keypair: Arc<Keypair>,
        ca_pem: Option<String>,
    ) -> Result<Self> {
        let mut clients = Vec::new();

        for (region, endpoint) in endpoints {
            let config = JitoConfig {
                endpoint: endpoint.clone(),
                ca_pem: ca_pem.clone(),
            };

            match JitoClient::new(config, Arc::clone(&auth_keypair)).await {
                Ok(client) => {
                    tracing::info!("Connected to Jito {} ({})", region, endpoint);
                    clients.push((region.clone(), Arc::new(client)));
                }
                Err(e) => {
                    tracing::warn!("Failed to connect to Jito {}: {}", region, e);
                    // Continue with other regions
                }
            }
        }

        if clients.is_empty() {
            anyhow::bail!("Failed to connect to any Jito block engine");
        }

        Ok(Self { clients, ca_pem })
    }

    /// Create clients for all known regions
    pub async fn all_regions(
        auth_keypair: Arc<Keypair>,
        ca_pem: Option<String>,
    ) -> Result<Self> {
        let endpoints: Vec<(String, String)> = JITO_ENDPOINTS
            .iter()
            .map(|(r, e)| (r.to_string(), e.to_string()))
            .collect();

        Self::new(&endpoints, auth_keypair, ca_pem).await
    }

    /// Submit bundle to all regions in parallel, return first success
    ///
    /// This is the key function for beating other searchers:
    /// - Submits to all regions simultaneously
    /// - Returns as soon as ANY region accepts
    /// - Other submissions continue in background (they'll dedupe on Jito's side)
    pub async fn submit_parallel(
        &self,
        transactions: Vec<VersionedTransaction>,
    ) -> Result<MultiRegionSubmitResult> {
        use tokio::sync::mpsc;

        let start = Instant::now();
        let (tx, mut rx) = mpsc::channel::<RegionResult>(self.clients.len());

        // Serialize transactions once
        let serialized: Vec<Vec<u8>> = transactions
            .iter()
            .map(|t| bincode::serialize(t))
            .collect::<Result<_, _>>()
            .context("Failed to serialize transactions")?;

        // Spawn parallel submissions
        for (region, client) in &self.clients {
            let region = region.clone();
            let client = Arc::clone(client);
            let serialized = serialized.clone();
            let tx = tx.clone();

            tokio::spawn(async move {
                let region_start = Instant::now();
                let result = client.send_bundle(serialized).await;
                let latency_ms = region_start.elapsed().as_millis() as u64;

                let region_result = RegionResult {
                    region: region.clone(),
                    result: result.map_err(|e| e.to_string()),
                    latency_ms,
                };

                // Ignore send error (receiver may have dropped after first success)
                let _ = tx.send(region_result).await;
            });
        }

        // Drop our sender so rx completes when all spawned tasks finish
        drop(tx);

        // Collect results, return on first success
        let mut all_results = Vec::new();
        let mut first_success: Option<MultiRegionSubmitResult> = None;

        while let Some(result) = rx.recv().await {
            if first_success.is_none() {
                if let Ok(ref bundle_id) = result.result {
                    first_success = Some(MultiRegionSubmitResult {
                        bundle_id: bundle_id.clone(),
                        first_region: result.region.clone(),
                        first_response_ms: start.elapsed().as_millis() as u64,
                        region_results: vec![result.clone()],
                    });
                    
                    tracing::info!(
                        region = %result.region,
                        latency_ms = result.latency_ms,
                        bundle_id = %bundle_id,
                        "Bundle accepted (first response)"
                    );
                }
            }
            all_results.push(result);
        }

        // Update first success with all results
        if let Some(mut success) = first_success {
            success.region_results = all_results;
            return Ok(success);
        }

        // All regions failed
        let errors: Vec<String> = all_results
            .iter()
            .map(|r| format!("{}: {:?}", r.region, r.result))
            .collect();

        anyhow::bail!("All regions failed: {}", errors.join(", "))
    }

    /// Submit to primary region only (for low-latency when confident)
    pub async fn submit_primary(
        &self,
        transactions: Vec<VersionedTransaction>,
    ) -> Result<String> {
        let (region, client) = self.clients
            .first()
            .ok_or_else(|| anyhow::anyhow!("No Jito clients available"))?;

        let bundle_id = client.send_versioned_bundle(transactions).await?;
        tracing::debug!(region = %region, bundle_id = %bundle_id, "Bundle submitted (primary only)");
        Ok(bundle_id)
    }

    /// Get all connected regions
    pub fn connected_regions(&self) -> Vec<&str> {
        self.clients.iter().map(|(r, _)| r.as_str()).collect()
    }

    /// Get primary client (for result streaming, etc.)
    pub fn primary_client(&self) -> Option<Arc<JitoClient>> {
        self.clients.first().map(|(_, c)| Arc::clone(c))
    }
}
