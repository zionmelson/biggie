//! Shred parsing with exact Agave-compatible wire format
//!
//! Shreds are the atomic unit of Turbine block propagation.
//! This module provides zero-copy parsing of raw UDP packets into structured data.

use std::fmt;

// ============================================================================
// Constants from Agave source (solana/ledger/src/shred.rs)
// ============================================================================

/// Size of Ed25519 signature
pub const SIZE_OF_SIGNATURE: usize = 64;

/// Size of the common shred header (shared by all shred types)
/// signature(64) + shred_variant(1) + slot(8) + index(4) + version(2) + fec_set_index(4) = 83
pub const SIZE_OF_COMMON_SHRED_HEADER: usize = 83;

/// Size of data shred headers (common + data-specific)  
/// common(83) + parent_offset(2) + flags(1) + size(2) = 88
pub const SIZE_OF_DATA_SHRED_HEADERS: usize = 88;

/// Size of coding shred headers (common + coding-specific)
/// common(83) + num_data_shreds(2) + num_coding_shreds(2) + position(2) = 89
pub const SIZE_OF_CODING_SHRED_HEADERS: usize = 89;

/// Payload size for data shreds
pub const DATA_SHRED_PAYLOAD_SIZE: usize = 1203;

/// Payload size for coding shreds  
pub const CODE_SHRED_PAYLOAD_SIZE: usize = 1228;

/// Data shreds per FEC block (erasure set)
pub const DATA_SHREDS_PER_FEC_BLOCK: usize = 32;

/// Coding shreds per FEC block (erasure set)
pub const CODING_SHREDS_PER_FEC_BLOCK: usize = 32;

/// Maximum shred size
pub const SHRED_SIZE: usize = 1228;

/// Size of Merkle root in shred
pub const SIZE_OF_MERKLE_ROOT: usize = 32;

/// Size of each Merkle proof entry
pub const SIZE_OF_MERKLE_PROOF_ENTRY: usize = 20;

// Wire format offsets (after signature)
const OFFSET_SHRED_VARIANT: usize = 64;
const OFFSET_SLOT: usize = 65;
const OFFSET_INDEX: usize = 73;
const OFFSET_VERSION: usize = 77;
const OFFSET_FEC_SET_INDEX: usize = 79;

// Data shred specific offsets
const OFFSET_DATA_PARENT_OFFSET: usize = 83;
const OFFSET_DATA_FLAGS: usize = 85;
const OFFSET_DATA_SIZE: usize = 86;
const OFFSET_DATA_PAYLOAD: usize = 88;

// Coding shred specific offsets
const OFFSET_CODE_NUM_DATA_SHREDS: usize = 83;
const OFFSET_CODE_NUM_CODING_SHREDS: usize = 85;
const OFFSET_CODE_POSITION: usize = 87;
const OFFSET_CODE_PAYLOAD: usize = 89;

// ============================================================================
// Shred Type and Variant
// ============================================================================

/// Type of shred (data vs coding)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShredType {
    Data,
    Code,
}

impl fmt::Display for ShredType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShredType::Data => write!(f, "Data"),
            ShredType::Code => write!(f, "Code"),
        }
    }
}

/// Shred variant encoding from the shred_variant byte
///
/// Format: upper 4 bits = type, lower 4 bits = proof_size (for Merkle shreds)
/// - 0b0110_xxxx = MerkleCode (proof_size = lower 4 bits)
/// - 0b0111_xxxx = MerkleCode resigned
/// - 0b1001_xxxx = MerkleData
/// - 0b1011_xxxx = MerkleData resigned
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShredVariant {
    pub shred_type: ShredType,
    pub proof_size: u8,
    pub resigned: bool,
}

impl ShredVariant {
    /// Parse the shred_variant byte
    pub fn from_byte(byte: u8) -> Option<Self> {
        let upper = byte >> 4;
        let proof_size = byte & 0x0F;

        match upper {
            0b0110 => Some(ShredVariant {
                shred_type: ShredType::Code,
                proof_size,
                resigned: false,
            }),
            0b0111 => Some(ShredVariant {
                shred_type: ShredType::Code,
                proof_size,
                resigned: true,
            }),
            0b1001 => Some(ShredVariant {
                shred_type: ShredType::Data,
                proof_size,
                resigned: false,
            }),
            0b1011 => Some(ShredVariant {
                shred_type: ShredType::Data,
                proof_size,
                resigned: true,
            }),
            _ => None, // Invalid or legacy shred variant
        }
    }
    
    /// Calculate data capacity for this shred variant
    /// This is the maximum size of the entry data that can fit in the shred
    pub fn data_capacity(&self) -> Option<usize> {
        match self.shred_type {
            ShredType::Data => {
                // capacity = SIZE_OF_PAYLOAD - SIZE_OF_HEADERS - SIZE_OF_MERKLE_ROOT
                //          - proof_size * SIZE_OF_MERKLE_PROOF_ENTRY
                //          - (resigned ? SIZE_OF_SIGNATURE : 0)
                let overhead = SIZE_OF_DATA_SHRED_HEADERS
                    + SIZE_OF_MERKLE_ROOT
                    + (self.proof_size as usize) * SIZE_OF_MERKLE_PROOF_ENTRY
                    + if self.resigned { SIZE_OF_SIGNATURE } else { 0 };
                DATA_SHRED_PAYLOAD_SIZE.checked_sub(overhead)
            }
            ShredType::Code => {
                // For coding shreds, capacity is similar calculation
                let overhead = SIZE_OF_CODING_SHRED_HEADERS
                    + SIZE_OF_MERKLE_ROOT
                    + (self.proof_size as usize) * SIZE_OF_MERKLE_PROOF_ENTRY
                    + if self.resigned { SIZE_OF_SIGNATURE } else { 0 };
                CODE_SHRED_PAYLOAD_SIZE.checked_sub(overhead)
            }
        }
    }
    
    /// Get the erasure shard region for this variant type
    /// Returns (start_offset, end_offset) within the raw shred bytes
    pub fn erasure_shard_offsets(&self) -> Option<(usize, usize)> {
        let capacity = self.data_capacity()?;
        match self.shred_type {
            ShredType::Data => {
                // Data shreds: erasure shard is [SIZE_OF_SIGNATURE..SIZE_OF_HEADERS+capacity]
                // This includes the headers (except signature) and the data
                Some((SIZE_OF_SIGNATURE, SIZE_OF_DATA_SHRED_HEADERS + capacity))
            }
            ShredType::Code => {
                // Coding shreds: erasure shard is [SIZE_OF_HEADERS..SIZE_OF_HEADERS+capacity]
                Some((SIZE_OF_CODING_SHRED_HEADERS, SIZE_OF_CODING_SHRED_HEADERS + capacity))
            }
        }
    }
}

// ============================================================================
// Header Structures
// ============================================================================

/// Common shred header (83 bytes)
#[derive(Debug, Clone, Copy)]
pub struct ShredCommonHeader {
    /// Ed25519 signature (64 bytes) - not parsed, we skip it
    pub signature_offset: usize,
    /// Shred type and proof size
    pub variant: ShredVariant,
    /// Slot this shred belongs to
    pub slot: u64,
    /// Index within the slot
    pub index: u32,
    /// Shred version
    pub version: u16,
    /// FEC set this shred belongs to
    pub fec_set_index: u32,
}

/// Data shred specific header (+5 bytes after common)
#[derive(Debug, Clone, Copy)]
pub struct DataShredHeader {
    /// Slot offset to parent block
    pub parent_offset: u16,
    /// Flags (bit 0 = SHRED_TICK_REFERENCE_MASK, bit 6 = DATA_COMPLETE_SHRED, bit 7 = LAST_SHRED_IN_SLOT)
    pub flags: u8,
    /// Size of payload data (includes headers)
    pub size: u16,
}

impl DataShredHeader {
    pub const FLAG_TICK_REFERENCE_MASK: u8 = 0b0011_1111;
    pub const FLAG_DATA_COMPLETE_SHRED: u8 = 0b0100_0000;
    pub const FLAG_LAST_SHRED_IN_SLOT: u8 = 0b1000_0000;

    #[inline]
    pub fn is_last_in_fec_set(&self) -> bool {
        (self.flags & Self::FLAG_DATA_COMPLETE_SHRED) != 0
    }

    #[inline]
    pub fn is_last_in_slot(&self) -> bool {
        (self.flags & Self::FLAG_LAST_SHRED_IN_SLOT) != 0
    }
}

/// Coding shred specific header (+6 bytes after common)
#[derive(Debug, Clone, Copy)]
pub struct CodingShredHeader {
    /// Number of data shreds in the FEC set
    pub num_data_shreds: u16,
    /// Number of coding shreds in the FEC set
    pub num_coding_shreds: u16,
    /// Position of this coding shred in the FEC set
    pub position: u16,
}

// ============================================================================
// Parsed Shred
// ============================================================================

/// A fully parsed shred
#[derive(Debug, Clone)]
pub struct ParsedShred {
    /// Common header
    pub common: ShredCommonHeader,
    /// Type-specific header and payload
    pub inner: ShredInner,
    /// Raw bytes (for FEC reconstruction)
    pub raw: Vec<u8>,
}

/// Type-specific shred data
#[derive(Debug, Clone)]
pub enum ShredInner {
    Data {
        header: DataShredHeader,
        /// Payload data (the actual block data)
        payload: Vec<u8>,
    },
    Code {
        header: CodingShredHeader,
        /// Erasure coding data
        payload: Vec<u8>,
    },
}

impl ParsedShred {
    /// Get the shred type
    pub fn shred_type(&self) -> ShredType {
        self.common.variant.shred_type
    }

    /// Get slot
    pub fn slot(&self) -> u64 {
        self.common.slot
    }

    /// Get index
    pub fn index(&self) -> u32 {
        self.common.index
    }

    /// Get FEC set index
    pub fn fec_set_index(&self) -> u32 {
        self.common.fec_set_index
    }

    /// Check if this is the last data shred in the FEC set
    pub fn is_last_in_fec_set(&self) -> bool {
        match &self.inner {
            ShredInner::Data { header, .. } => header.is_last_in_fec_set(),
            ShredInner::Code { .. } => false,
        }
    }

    /// Check if this is the last shred in the slot
    pub fn is_last_in_slot(&self) -> bool {
        match &self.inner {
            ShredInner::Data { header, .. } => header.is_last_in_slot(),
            ShredInner::Code { .. } => false,
        }
    }

    /// Get payload for data shreds
    pub fn data_payload(&self) -> Option<&[u8]> {
        match &self.inner {
            ShredInner::Data { payload, .. } => Some(payload),
            ShredInner::Code { .. } => None,
        }
    }

    /// Get coding shred header
    pub fn coding_header(&self) -> Option<&CodingShredHeader> {
        match &self.inner {
            ShredInner::Code { header, .. } => Some(header),
            ShredInner::Data { .. } => None,
        }
    }
    
    /// Get the erasure shard bytes for Reed-Solomon coding
    /// This is the region of the shred that participates in FEC
    pub fn erasure_shard(&self) -> Option<&[u8]> {
        let (start, end) = self.common.variant.erasure_shard_offsets()?;
        if end <= self.raw.len() {
            Some(&self.raw[start..end])
        } else {
            // Shred is too short, use what we have
            if start < self.raw.len() {
                Some(&self.raw[start..])
            } else {
                None
            }
        }
    }
    
    /// Get the entry data bytes (the actual payload, excluding headers and FEC overhead)
    /// For data shreds only, returns the deserialized entry data
    pub fn entry_data(&self) -> Option<&[u8]> {
        match &self.inner {
            ShredInner::Data { header, .. } => {
                // The size field indicates total bytes including headers
                let size = header.size as usize;
                if size > SIZE_OF_DATA_SHRED_HEADERS && size <= self.raw.len() {
                    Some(&self.raw[SIZE_OF_DATA_SHRED_HEADERS..size])
                } else {
                    None
                }
            }
            ShredInner::Code { .. } => None,
        }
    }
}

// ============================================================================
// Parser
// ============================================================================

/// Error type for shred parsing
#[derive(Debug, Clone)]
pub enum ParseError {
    TooShort { expected: usize, actual: usize },
    InvalidVariant(u8),
    InvalidSize { header_size: u16, actual: usize },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::TooShort { expected, actual } => {
                write!(
                    f,
                    "shred too short: expected {} bytes, got {}",
                    expected, actual
                )
            }
            ParseError::InvalidVariant(v) => {
                write!(f, "invalid shred variant: 0x{:02x}", v)
            }
            ParseError::InvalidSize {
                header_size,
                actual,
            } => {
                write!(
                    f,
                    "invalid size in header: {} vs actual {}",
                    header_size, actual
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Extract entry data from raw data shred bytes
/// This is the data portion that contains serialized entries
/// Returns None if the shred is invalid or not a data shred
pub fn extract_entry_data(raw: &[u8]) -> Option<&[u8]> {
    if raw.len() < SIZE_OF_DATA_SHRED_HEADERS {
        return None;
    }
    
    // Check shred type
    let variant_byte = raw.get(OFFSET_SHRED_VARIANT)?;
    let variant = ShredVariant::from_byte(*variant_byte)?;
    
    if variant.shred_type != ShredType::Data {
        return None;
    }
    
    // Get the size field from the data header
    let size = u16::from_le_bytes(
        raw.get(OFFSET_DATA_SIZE..OFFSET_DATA_SIZE + 2)?
            .try_into()
            .ok()?
    ) as usize;
    
    // Validate size is reasonable
    let capacity = variant.data_capacity()?;
    let max_size = SIZE_OF_DATA_SHRED_HEADERS + capacity;
    
    if size < SIZE_OF_DATA_SHRED_HEADERS || size > max_size || size > raw.len() {
        return None;
    }
    
    // Extract the data portion
    Some(&raw[SIZE_OF_DATA_SHRED_HEADERS..size])
}

/// Parse a raw shred packet
///
/// This is the main entry point for shred parsing. It performs zero-copy parsing
/// of the headers and copies only the payload data.
pub fn parse_shred(data: &[u8]) -> Result<ParsedShred, ParseError> {
    // Check minimum size (need at least common header to determine type)
    if data.len() < SIZE_OF_COMMON_SHRED_HEADER {
        return Err(ParseError::TooShort {
            expected: SIZE_OF_COMMON_SHRED_HEADER,
            actual: data.len(),
        });
    }

    // Parse variant byte to determine shred type
    let variant_byte = data[OFFSET_SHRED_VARIANT];
    let variant =
        ShredVariant::from_byte(variant_byte).ok_or(ParseError::InvalidVariant(variant_byte))?;

    // Parse common header fields (all little-endian)
    let slot = u64::from_le_bytes(data[OFFSET_SLOT..OFFSET_SLOT + 8].try_into().unwrap());
    let index = u32::from_le_bytes(data[OFFSET_INDEX..OFFSET_INDEX + 4].try_into().unwrap());
    let version = u16::from_le_bytes(data[OFFSET_VERSION..OFFSET_VERSION + 2].try_into().unwrap());
    let fec_set_index = u32::from_le_bytes(
        data[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4]
            .try_into()
            .unwrap(),
    );

    let common = ShredCommonHeader {
        signature_offset: 0,
        variant,
        slot,
        index,
        version,
        fec_set_index,
    };

    // Parse type-specific header and payload
    let inner = match variant.shred_type {
        ShredType::Data => {
            if data.len() < SIZE_OF_DATA_SHRED_HEADERS {
                return Err(ParseError::TooShort {
                    expected: SIZE_OF_DATA_SHRED_HEADERS,
                    actual: data.len(),
                });
            }

            let parent_offset = u16::from_le_bytes(
                data[OFFSET_DATA_PARENT_OFFSET..OFFSET_DATA_PARENT_OFFSET + 2]
                    .try_into()
                    .unwrap(),
            );
            let flags = data[OFFSET_DATA_FLAGS];
            let size = u16::from_le_bytes(
                data[OFFSET_DATA_SIZE..OFFSET_DATA_SIZE + 2]
                    .try_into()
                    .unwrap(),
            );

            let header = DataShredHeader {
                parent_offset,
                flags,
                size,
            };

            // Validate size field - it should be between SIZE_OF_HEADERS and SIZE_OF_HEADERS+capacity
            let capacity = variant.data_capacity().unwrap_or(0);
            let max_valid_size = SIZE_OF_DATA_SHRED_HEADERS + capacity;
            
            // Payload starts after header, size field indicates total including headers
            // If size is invalid, fall back to extracting up to capacity
            let payload_end = if size as usize >= SIZE_OF_DATA_SHRED_HEADERS 
                && size as usize <= max_valid_size 
            {
                (size as usize).min(data.len())
            } else {
                // Size field is garbage, use capacity instead
                max_valid_size.min(data.len())
            };
            
            let payload = if payload_end > OFFSET_DATA_PAYLOAD {
                data[OFFSET_DATA_PAYLOAD..payload_end].to_vec()
            } else {
                Vec::new()
            };

            ShredInner::Data { header, payload }
        }
        ShredType::Code => {
            if data.len() < SIZE_OF_CODING_SHRED_HEADERS {
                return Err(ParseError::TooShort {
                    expected: SIZE_OF_CODING_SHRED_HEADERS,
                    actual: data.len(),
                });
            }

            let num_data_shreds = u16::from_le_bytes(
                data[OFFSET_CODE_NUM_DATA_SHREDS..OFFSET_CODE_NUM_DATA_SHREDS + 2]
                    .try_into()
                    .unwrap(),
            );
            let num_coding_shreds = u16::from_le_bytes(
                data[OFFSET_CODE_NUM_CODING_SHREDS..OFFSET_CODE_NUM_CODING_SHREDS + 2]
                    .try_into()
                    .unwrap(),
            );
            let position = u16::from_le_bytes(
                data[OFFSET_CODE_POSITION..OFFSET_CODE_POSITION + 2]
                    .try_into()
                    .unwrap(),
            );

            let header = CodingShredHeader {
                num_data_shreds,
                num_coding_shreds,
                position,
            };

            // Coding payload is fixed size
            let payload_end = data
                .len()
                .min(OFFSET_CODE_PAYLOAD + CODE_SHRED_PAYLOAD_SIZE);
            let payload = if payload_end > OFFSET_CODE_PAYLOAD {
                data[OFFSET_CODE_PAYLOAD..payload_end].to_vec()
            } else {
                Vec::new()
            };

            ShredInner::Code { header, payload }
        }
    };

    Ok(ParsedShred {
        common,
        inner,
        raw: data.to_vec(),
    })
}

/// Quick peek at shred header without full parse
/// Returns (shred_type, slot, index, fec_set_index)
#[inline]
pub fn peek_shred_header(data: &[u8]) -> Option<(ShredType, u64, u32, u32)> {
    if data.len() < SIZE_OF_COMMON_SHRED_HEADER {
        return None;
    }

    let variant = ShredVariant::from_byte(data[OFFSET_SHRED_VARIANT])?;
    let slot = u64::from_le_bytes(data[OFFSET_SLOT..OFFSET_SLOT + 8].try_into().ok()?);
    let index = u32::from_le_bytes(data[OFFSET_INDEX..OFFSET_INDEX + 4].try_into().ok()?);
    let fec_set_index = u32::from_le_bytes(
        data[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4]
            .try_into()
            .ok()?,
    );

    Some((variant.shred_type, slot, index, fec_set_index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shred_variant_parsing() {
        // MerkleData
        assert_eq!(
            ShredVariant::from_byte(0x90),
            Some(ShredVariant {
                shred_type: ShredType::Data,
                proof_size: 0,
                resigned: false,
            })
        );

        // MerkleData with proof_size = 5
        assert_eq!(
            ShredVariant::from_byte(0x95),
            Some(ShredVariant {
                shred_type: ShredType::Data,
                proof_size: 5,
                resigned: false,
            })
        );

        // MerkleData resigned
        assert_eq!(
            ShredVariant::from_byte(0xB0),
            Some(ShredVariant {
                shred_type: ShredType::Data,
                proof_size: 0,
                resigned: true,
            })
        );

        // MerkleCode
        assert_eq!(
            ShredVariant::from_byte(0x60),
            Some(ShredVariant {
                shred_type: ShredType::Code,
                proof_size: 0,
                resigned: false,
            })
        );

        // Invalid
        assert_eq!(ShredVariant::from_byte(0x00), None);
        assert_eq!(ShredVariant::from_byte(0xFF), None);
    }

    #[test]
    fn test_constants() {
        // Verify our constants match expected values
        assert_eq!(SIZE_OF_COMMON_SHRED_HEADER, 83);
        assert_eq!(SIZE_OF_DATA_SHRED_HEADERS, 88);
        assert_eq!(SIZE_OF_CODING_SHRED_HEADERS, 89);
        assert_eq!(DATA_SHRED_PAYLOAD_SIZE, 1203);
        assert_eq!(CODE_SHRED_PAYLOAD_SIZE, 1228);
    }
}
