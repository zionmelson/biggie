//! Shredstream/Geyser MEV pipeline modules
//!
//! This module provides transaction parsing and MEV detection for
//! low-latency backrunning from shredstream or Geyser data.
//!
//! Architecture:
//! ```text
//! Geyser TransactionUpdates → Parser → Evaluator → Bundle Builder → Jito Submitter
//! (or legacy: Shredstream Proxy → Parser → ...)
//! ```

pub mod backrun;
pub mod evaluator;
pub mod geyser_pipeline;
pub mod parser;
pub mod pipeline;
pub mod programs;
pub mod types;

// Core parsing types
pub use parser::{ParsedSwap, SwapDirection, TransactionSwaps};
pub use programs::{get_dex_type, is_swap_instruction, DexType};

// Shredstream client types (legacy)
pub use types::{ShredstreamConfig, ShredstreamMonitor, ShredstreamStats, ShredTransaction};

// MEV evaluation
pub use evaluator::{MevEvaluator, MevOpportunity, PoolInfo};

// Bundle building
pub use backrun::{BackrunBundle, BackrunBundleBuilder, BundleBuilderConfig};

// Geyser pipeline (preferred)
pub use geyser_pipeline::{
    spawn_geyser_pipeline, GeyserMevPipeline, GeyserPipelineConfig, GeyserPipelineStats,
};

// Main pipeline
pub use pipeline::{spawn_pipeline, BlockhashUpdate, MevPipeline, PipelineConfig, PipelineStats};
