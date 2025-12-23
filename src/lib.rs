pub mod amm_math;
pub mod clmm_price_map;
pub mod config;
pub mod constants;
pub mod executor;
pub mod jito;
pub mod shredstream;

// Geyser client - primary data source for local validator
pub mod geyser;

// Dynamic pool discovery from Geyser (replaces static JSON maps)
pub mod pool_discovery;
