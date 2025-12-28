//! ShredStream + Geyser Unified Arbitrage Pipeline
//!
//! # Architecture
//!
//! ```text
//! Turbine (fast state) ────┐
//!                          ├──► Pool State Manager ──► Arb Evaluator ──► Bundle Builder ──► Jito
//! Geyser (ground truth) ───┘
//! ```
//!
//! The unified pipeline uses:
//! - **Turbine/ShredStream** for fast state updates (~0.5-2ms after block)
//! - **Geyser** for ground truth state confirmation (~5-15ms)
//! - **Pool State Manager** to track vault balances from both sources

pub mod arb_bundle;
pub mod evaluator;
pub mod parser;
pub mod programs;
pub mod unified_pipeline;

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
