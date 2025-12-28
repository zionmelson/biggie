//! ShredStream + Geyser Unified Arbitrage Pipeline
//!
//! # Geyser-First Architecture
//!
//! ```text
//! Geyser (sole state source) ──► Pool State Manager ──► Arb Evaluator ──► Bundle Builder ──► Jito
//!                                        ▲
//! Shreds (activity signals) ─────────────┘ (prioritization hints, not state updates)
//! ```
//!
//! ## Design Philosophy
//!
//! **Geyser is the single source of truth for all pool state.**
//!
//! Attempting to simulate pool state from parsed swap transactions is error-prone:
//! - Each DEX has different instruction layouts and account ordering
//! - Simulation math must exactly match on-chain behavior
//! - Edge cases: partial fills, failed inner TXs, Token-2022 fees, CLMM tick crossings
//! - Every DEX upgrade requires parser updates
//!
//! ## What Shreds Provide
//!
//! Instead of state simulation, shreds serve as **activity signals**:
//! - **Early opportunity signaling**: Know a pool had activity ~5-10ms before Geyser confirms
//! - **Pre-warming**: Prepare tick/bin arrays for pools that are about to update
//! - **Latency metrics**: Track shred-to-Geyser delay for health monitoring
//!
//! ## Timing Reality
//!
//! ```text
//! t=0ms      Swap executed in block
//! t=0.5-2ms  Shred arrives → emit PoolActivitySignal (NO state update)
//! t=5-15ms   Geyser vault update → update state → evaluate arb → submit
//! ```

pub mod arb_bundle;
pub mod evaluator;
pub mod parser;
pub mod programs;
pub mod unified_pipeline;

use solana_sdk::pubkey::Pubkey;
use std::time::Instant;

// ============================================================================
// Pool Activity Signal (Shred-based signaling, NOT state updates)
// ============================================================================

/// A signal that a pool had swap activity, detected from shreds.
///
/// This is used for **prioritization and pre-warming**, NOT for state updates.
/// The actual state update comes from Geyser ~5-15ms later.
///
/// # Use Cases
/// - **Priority hints**: When Geyser update arrives, prioritize "hot" pools
/// - **Pre-warming**: Fetch tick/bin arrays for pools likely to need them
/// - **Latency tracking**: Measure shred-to-Geyser delay for health monitoring
#[derive(Debug, Clone)]
pub struct PoolActivitySignal {
    /// The pool that had activity
    pub pool: Pubkey,
    /// When the shred was received (for latency tracking)
    pub shred_received_at: Instant,
    /// Slot where activity was detected
    pub slot: u64,
    /// Hint about swap direction (if parseable, for pre-warming tick arrays)
    pub direction_hint: Option<SwapDirection>,
    /// Number of swaps detected on this pool in this entry batch
    pub swap_count: u32,
}

impl PoolActivitySignal {
    pub fn new(pool: Pubkey, slot: u64) -> Self {
        Self {
            pool,
            shred_received_at: Instant::now(),
            slot,
            direction_hint: None,
            swap_count: 1,
        }
    }

    pub fn with_direction(mut self, direction: SwapDirection) -> Self {
        self.direction_hint = Some(direction);
        self
    }

    /// Calculate latency from shred to now (call when Geyser confirms)
    pub fn latency_since_shred(&self) -> std::time::Duration {
        self.shred_received_at.elapsed()
    }
}

// Core parsing types
pub use parser::{ParsedSwap, SwapDirection, TransactionSwaps};
pub use programs::{get_dex_type, is_swap_instruction, DexType};

// Arbitrage evaluation
pub use evaluator::{ArbEvaluator, ArbOpportunity, PoolInfo, PoolSwapState};

// Bundle building
pub use arb_bundle::{ArbBundle, ArbBundleBuilder, BundleBuilderConfig};

// Unified pipeline
pub use unified_pipeline::{
    spawn_unified_pipeline, UnifiedArbPipeline, UnifiedPipelineConfig, UnifiedPipelineStats,
};
