//! Integration test for Jito pipeline connectivity.
//!
//! This binary tests:
//! 1. Authentication with Jito block engine
//! 2. Leader schedule polling
//! 3. Bundle results subscription
//!
//! Run with: cargo run --bin test_jito_pipeline

use anyhow::{anyhow, Result};
use futures::StreamExt;
use solana_sdk::signer::Signer;
use std::sync::Arc;
use std::time::Duration;
use tre_arb_bot::config::Config;
use tre_arb_bot::jito::{BundleStatus, JitoClient, JitoConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("tre_arb_bot=info".parse()?)
                .add_directive("test_jito_pipeline=info".parse()?),
        )
        .init();

    tracing::info!("Starting Jito pipeline integration test");

    // Load config
    let config = Config::load(std::path::Path::new("config.json")).map_err(|e| anyhow!("{}", e))?;
    tracing::info!("Loaded config");

    // Load auth keypair
    let auth_keypair = Arc::new(
        config
            .wallets
            .auth
            .load_keypair()
            .map_err(|e| anyhow!("{}", e))?,
    );
    tracing::info!("Auth pubkey: {}", auth_keypair.pubkey());

    // Create Jito client
    let jito_config = JitoConfig {
        endpoint: config.jito.primary_endpoint(),
        ca_pem: config.jito.ca_pem.clone(),
    };
    tracing::info!("Connecting to Jito at {}", jito_config.endpoint);

    let client = JitoClient::new(jito_config, auth_keypair).await?;
    tracing::info!("JitoClient created");

    // Test 1: Authenticate
    tracing::info!("Test 1: Authenticating...");
    client.authenticate().await?;
    tracing::info!("✓ Authentication successful");

    // Test 2: Get next scheduled leader
    tracing::info!("Test 2: Getting next scheduled leader...");
    let leader = client.get_next_scheduled_leader().await?;
    tracing::info!(
        "✓ Next leader: {} in {} slots (region: {})",
        leader.next_leader_identity,
        leader.next_leader_slot - leader.current_slot,
        leader.next_leader_region
    );

    // Test 3: Subscribe to bundle results (with timeout)
    tracing::info!("Test 3: Subscribing to bundle results stream...");
    let mut stream = client.subscribe_bundle_results().await?;
    tracing::info!("✓ Bundle results stream connected");

    tracing::info!("Listening for bundle results for 5 seconds...");
    let listen_result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut count = 0;
        while let Some(result) = stream.next().await {
            match result {
                Ok(bundle_result) => {
                    let status = BundleStatus::from(bundle_result);
                    tracing::info!("Received bundle result: {:?}", status);
                    count += 1;
                }
                Err(e) => {
                    tracing::warn!("Stream error: {}", e);
                    break;
                }
            }
        }
        count
    })
    .await;

    match listen_result {
        Ok(count) => {
            tracing::info!("Stream ended after receiving {} bundle results", count);
        }
        Err(_) => {
            tracing::info!("✓ Stream timeout reached (expected - no bundles sent)");
        }
    }

    // Test 4: Verify stream can be reconnected
    tracing::info!("Test 4: Reconnecting to bundle results stream...");
    let _stream2 = client.subscribe_bundle_results().await?;
    tracing::info!("✓ Stream reconnection successful");

    tracing::info!("");
    tracing::info!("═══════════════════════════════════════════════════════");
    tracing::info!("  All Jito pipeline tests passed!");
    tracing::info!("═══════════════════════════════════════════════════════");

    Ok(())
}
