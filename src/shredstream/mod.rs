//! ShredStream + Geyser Unified MEV Pipeline
//!
//! # Architecture
//!
//! ```text
//! ShredStream (early TX) ──┐
//!                          ├──► PredictiveStateManager ──► MEV Evaluator ──► Bundle Builder ──► Jito
//! Geyser (ground truth) ───┘
//! ```
//!
//! The unified pipeline uses:
//! - **ShredStream** for early transaction detection (before confirmation)
//! - **Geyser** for ground truth state confirmation
//! - **PredictiveStateManager** to bridge the gap between early detection and confirmation

pub mod backrun;
pub mod evaluator;
pub mod parser;
pub mod programs;
pub mod unified_pipeline;

// Core parsing types
pub use parser::{ParsedSwap, SwapDirection, TransactionSwaps};
pub use programs::{get_dex_type, is_swap_instruction, DexType};

// MEV evaluation
pub use evaluator::{MevEvaluator, MevOpportunity, PoolInfo};

// Bundle building
pub use backrun::{BackrunBundle, BackrunBundleBuilder, BundleBuilderConfig};

// Unified pipeline
pub use unified_pipeline::{
    spawn_unified_pipeline, UnifiedMevPipeline, UnifiedPipelineConfig, UnifiedPipelineStats,
};
