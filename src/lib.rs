pub mod amm_math;
pub mod clmm_math;
pub mod config;
pub mod constants;
pub mod dlmm_math;
pub mod executor;
pub mod jito;
pub mod pools;
pub mod shredstream;

// Geyser client - primary data source for local validator
pub mod geyser;

// Tick/Bin array caches for accurate CLMM/DLMM simulation
// Populated via Geyser subscriptions to tick/bin array accounts
pub mod tick_array_cache;
pub mod bin_array_cache;

// Dynamic pool discovery from Geyser (replaces static JSON maps)
pub mod pool_discovery;

// Jito tip floor monitoring for competitive tipping
pub mod tip_monitor;

// Unified predictive state management (ShredStream + Geyser integration)
pub mod predictive_state;

// Multi-region parallel bundle submission for maximum competitiveness
pub mod multi_region;

// Direct Turbine UDP deshredding - bypasses ShredStream proxy for minimum latency
// Receives shreds directly from local validator's TVU broadcast
pub mod turbine;
