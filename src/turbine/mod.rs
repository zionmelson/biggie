//! Direct Turbine UDP Deshredding
//!
//! Receives shreds directly from the local validator's Turbine broadcast,
//! bypassing the ShredStream proxy for minimum latency.
//!
//! # Architecture
//!
//! ```text
//! Validator Turbine (UDP broadcast)
//!          │
//!          ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │              UDP Shred Receiver                          │
//! │  • Binds to TVU port (default 8001)                     │
//! │  • Receives raw shred packets                           │
//! │  • Zero-copy packet handling                            │
//! └─────────────────────────────────────────────────────────┘
//!          │
//!          ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │              Shred Parser                                │
//! │  • Parse shred header (slot, index, type)               │
//! │  • Identify data vs coding shreds                       │
//! │  • Route to FEC set assembly                            │
//! └─────────────────────────────────────────────────────────┘
//!          │
//!          ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │              FEC Set Assembler                           │
//! │  • Group shreds by (slot, fec_set_index)                │
//! │  • Track data and coding shreds                         │
//! │  • Trigger reconstruction when threshold reached        │
//! └─────────────────────────────────────────────────────────┘
//!          │
//!          ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │              Reed-Solomon Decoder                        │
//! │  • Reconstruct missing data shreds from coding shreds   │
//! │  • Uses erasure coding for fault tolerance              │
//! └─────────────────────────────────────────────────────────┘
//!          │
//!          ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │              Entry Assembler                             │
//! │  • Concatenate data shred payloads                      │
//! │  • Deserialize into Vec<Entry>                          │
//! │  • Send to MEV pipeline                                 │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # Shred Format (from Agave source)
//!
//! ## Common Header (83 bytes)
//! - signature: 64 bytes (Ed25519 signature)  
//! - shred_variant: 1 byte (encodes type + proof_size + resigned)
//! - slot: 8 bytes (u64)
//! - index: 4 bytes (u32)
//! - version: 2 bytes (u16)
//! - fec_set_index: 4 bytes (u32)
//!
//! ## Data Shred Header (+5 bytes = 88 total)
//! - parent_offset: 2 bytes (u16)
//! - flags: 1 byte (ShredFlags)
//! - size: 2 bytes (u16) - total size including headers
//!
//! ## Coding Shred Header (+6 bytes = 89 total)
//! - num_data_shreds: 2 bytes (u16)
//! - num_coding_shreds: 2 bytes (u16)
//! - position: 2 bytes (u16)
//!
//! ## Shred Variant Encoding (1 byte)
//! - 0b0110_????  MerkleCode (proof_size in lower 4 bits)
//! - 0b0111_????  MerkleCode resigned
//! - 0b1001_????  MerkleData  
//! - 0b1011_????  MerkleData resigned
//!
//! ## FEC Parameters
//! - DATA_SHREDS_PER_FEC_BLOCK: 32
//! - CODING_SHREDS_PER_FEC_BLOCK: 32
//! - Total: 64 shreds per FEC set
//!
//! # Latency Advantage
//!
//! ShredStream Proxy: Validator → Jito Relay → Proxy → gRPC → Bot (~5-10ms)
//! Direct UDP:        Validator → UDP → Bot (~0.5-2ms)
//!
//! This 5-8ms advantage is critical for winning MEV auctions.

pub mod assembler;
pub mod fec;
pub mod integration;
pub mod receiver;
pub mod shred;

pub use assembler::{run_entry_assembler, AssembledEntries, EntryAssembler};
pub use fec::{CompleteFecSet, ErasureSetId, FecSet, FecSetAssembler, FecAssemblerConfig};
pub use integration::{
    HybridPipeline, HybridPipelineConfig, SourcedTransaction, TransactionSource, TxDeduplicator,
};
pub use receiver::{TurbineConfig, TurbineReceiver, TurbineStats, TurbineStatsSnapshot};
pub use shred::{
    parse_shred, CodingShredHeader, DataShredHeader, ParsedShred, ShredCommonHeader, ShredType,
    ShredVariant, CODING_SHREDS_PER_FEC_BLOCK, DATA_SHREDS_PER_FEC_BLOCK,
};
