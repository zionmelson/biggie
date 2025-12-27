pub mod amm_math;
pub mod clmm_math;
pub mod config;
pub mod constants;
pub mod dlmm_math;
pub mod executor;
pub mod jito;
pub mod pools;
pub mod shredstream;

// Geyser client - authoritative state source for vault balances
pub mod geyser;

// Tick/Bin array caches for accurate CLMM/DLMM simulation
// Populated via Geyser subscriptions to tick/bin array accounts
pub mod tick_array_cache;
pub mod bin_array_cache;

// Dynamic pool discovery from Geyser (replaces static JSON maps)
pub mod pool_discovery;

// Jito tip floor monitoring for competitive tipping
pub mod tip_monitor;

// Pool state manager - single source of truth for pool reserves
// Updated by Turbine (fast) and Geyser (authoritative)
// Triggers arb evaluation on every state change
pub mod pool_state;

// Multi-region parallel bundle submission for maximum competitiveness
pub mod multi_region;

// Direct Turbine UDP reception - fastest way to learn about block contents
// Receives shreds directly from validator's TVU broadcast (~0.5-2ms latency)
// Note: Shreds are POST-EXECUTION data, not pending transactions
pub mod turbine;
