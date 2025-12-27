//! FEC Set Assembly and Reed-Solomon Reconstruction
//!
//! Collects shreds belonging to the same FEC set and reconstructs missing
//! data shreds using erasure coding when enough shreds are available.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, warn, trace};
use reed_solomon_erasure::galois_8::ReedSolomon;

use crate::turbine::shred::{
    ParsedShred, ShredType, ShredInner, ShredVariant,
    DATA_SHREDS_PER_FEC_BLOCK, CODING_SHREDS_PER_FEC_BLOCK,
    DATA_SHRED_PAYLOAD_SIZE,
    SIZE_OF_SIGNATURE, SIZE_OF_DATA_SHRED_HEADERS, SIZE_OF_CODING_SHRED_HEADERS,
    extract_entry_data,
};

// ============================================================================
// Types
// ============================================================================

/// Unique identifier for an FEC set (erasure set)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErasureSetId {
    pub slot: u64,
    pub fec_set_index: u32,
}

impl ErasureSetId {
    pub fn new(slot: u64, fec_set_index: u32) -> Self {
        Self { slot, fec_set_index }
    }
}

/// A complete FEC set ready for entry extraction
#[derive(Debug)]
pub struct CompleteFecSet {
    pub id: ErasureSetId,
    /// Data shred payloads in index order
    pub data_payloads: Vec<Vec<u8>>,
    /// Whether this is the last FEC set in the slot
    pub is_last_in_slot: bool,
    /// Number of data shreds in this set
    pub num_data_shreds: usize,
}

/// Tracks shreds for a single FEC set
pub struct FecSet {
    pub id: ErasureSetId,
    /// Data shreds by index (relative to FEC set, 0..num_data_shreds)
    data_shreds: HashMap<u32, ParsedShred>,
    /// Coding shreds by position
    coding_shreds: HashMap<u16, ParsedShred>,
    /// Number of data shreds expected (from coding header)
    num_data_shreds: Option<u16>,
    /// Number of coding shreds expected (from coding header)
    num_coding_shreds: Option<u16>,
    /// Whether we've seen the "last shred in slot" flag
    is_last_in_slot: bool,
    /// Whether we've seen the "data complete" flag
    is_data_complete: bool,
    /// Creation time for expiry
    created_at: Instant,
}

impl FecSet {
    pub fn new(id: ErasureSetId) -> Self {
        Self {
            id,
            data_shreds: HashMap::with_capacity(DATA_SHREDS_PER_FEC_BLOCK),
            coding_shreds: HashMap::with_capacity(CODING_SHREDS_PER_FEC_BLOCK),
            num_data_shreds: None,
            num_coding_shreds: None,
            is_last_in_slot: false,
            is_data_complete: false,
            created_at: Instant::now(),
        }
    }
    
    /// Add a shred to this FEC set
    /// Returns true if the set is now complete
    pub fn add_shred(&mut self, shred: ParsedShred) -> bool {
        // Compute relative index within FEC set
        let relative_index = shred.index().saturating_sub(self.id.fec_set_index);
        
        match &shred.inner {
            ShredInner::Data { header, .. } => {
                // Update flags
                if header.is_last_in_slot() {
                    self.is_last_in_slot = true;
                }
                if header.is_last_in_fec_set() {
                    self.is_data_complete = true;
                    // The last data shred index tells us how many data shreds
                    self.num_data_shreds = Some((relative_index + 1) as u16);
                }
                
                self.data_shreds.insert(relative_index, shred);
            }
            ShredInner::Code { header, .. } => {
                // Coding headers tell us the FEC set size
                self.num_data_shreds = Some(header.num_data_shreds);
                self.num_coding_shreds = Some(header.num_coding_shreds);
                
                self.coding_shreds.insert(header.position, shred);
            }
        }
        
        self.is_complete()
    }
    
    /// Check if we have enough shreds to reconstruct all data
    pub fn is_complete(&self) -> bool {
        let Some(num_data) = self.num_data_shreds else {
            return false;
        };
        
        // If we have all data shreds, we're done
        if self.data_shreds.len() >= num_data as usize {
            return true;
        }
        
        // Otherwise, check if we have enough for Reed-Solomon reconstruction
        // Need at least num_data_shreds total (data + coding combined)
        let total_received = self.data_shreds.len() + self.coding_shreds.len();
        total_received >= num_data as usize
    }
    
    /// Check if we can potentially complete (have FEC parameters)
    pub fn has_fec_params(&self) -> bool {
        self.num_data_shreds.is_some() && self.num_coding_shreds.is_some()
    }
    
    /// Attempt to reconstruct missing data shreds
    /// Returns None if reconstruction is not possible
    pub fn try_reconstruct(&self) -> Option<CompleteFecSet> {
        let num_data = self.num_data_shreds? as usize;
        
        // If we already have all data shreds, just return them
        if self.data_shreds.len() >= num_data {
            let mut payloads = vec![Vec::new(); num_data];
            for (&idx, shred) in &self.data_shreds {
                if (idx as usize) < num_data {
                    // Use entry_data() which properly validates the size field
                    if let Some(data) = shred.entry_data() {
                        payloads[idx as usize] = data.to_vec();
                    } else if let ShredInner::Data { payload, .. } = &shred.inner {
                        // Fallback to parsed payload
                        payloads[idx as usize] = payload.clone();
                    }
                }
            }
            
            return Some(CompleteFecSet {
                id: self.id,
                data_payloads: payloads,
                is_last_in_slot: self.is_last_in_slot,
                num_data_shreds: num_data,
            });
        }
        
        // Need Reed-Solomon reconstruction
        // For now, we'll skip actual RS decoding and require all data shreds
        // TODO: Implement proper Reed-Solomon reconstruction with reed-solomon-simd
        
        let num_coding = self.num_coding_shreds? as usize;
        let total_received = self.data_shreds.len() + self.coding_shreds.len();
        
        if total_received < num_data {
            // Not enough shreds for reconstruction
            trace!(
                slot = self.id.slot,
                fec_set = self.id.fec_set_index,
                data_received = self.data_shreds.len(),
                coding_received = self.coding_shreds.len(),
                need = num_data,
                "insufficient shreds for FEC reconstruction"
            );
            return None;
        }
        
        // Perform Reed-Solomon reconstruction
        self.reed_solomon_reconstruct(num_data, num_coding)
    }
    
    /// Reed-Solomon erasure decoding
    /// 
    /// IMPORTANT: Solana's RS coding operates on the "erasure shard" region of each shred:
    /// - Data shreds: bytes [64..88+capacity] (signature excluded, headers + data included)
    /// - Coding shreds: bytes [89..89+capacity] (all headers excluded)
    /// 
    /// After RS reconstruction, we need to re-parse the recovered bytes to extract entry data.
    fn reed_solomon_reconstruct(&self, num_data: usize, num_coding: usize) -> Option<CompleteFecSet> {
        // Get proof_size from any available shred to calculate shard size
        let proof_size = self.data_shreds.values().next()
            .map(|s| s.common.variant.proof_size)
            .or_else(|| self.coding_shreds.values().next().map(|s| s.common.variant.proof_size))?;
        
        // Calculate the erasure shard size based on proof_size
        // This is the size of the region that participates in RS coding
        let variant = ShredVariant { 
            shred_type: ShredType::Data, 
            proof_size, 
            resigned: false  // We don't know, assume false for size calculation
        };
        let capacity = variant.data_capacity()?;
        
        // Erasure shard for data shreds: [SIZE_OF_SIGNATURE..SIZE_OF_DATA_SHRED_HEADERS+capacity]
        // = [64..88+capacity]
        let data_shard_size = SIZE_OF_DATA_SHRED_HEADERS - SIZE_OF_SIGNATURE + capacity;
        // = 88 - 64 + capacity = 24 + capacity
        
        let total_shards = num_data + num_coding;
        
        // Build shards array using proper erasure shard region
        let mut shards: Vec<Option<Vec<u8>>> = vec![None; total_shards];
        
        // Also keep track of raw shreds for data extraction later
        let mut raw_shreds: Vec<Option<Vec<u8>>> = vec![None; num_data];
        
        // Fill in data shreds we have
        for (&idx, shred) in &self.data_shreds {
            let idx = idx as usize;
            if idx < num_data {
                // Extract erasure shard from raw bytes
                let (start, end) = shred.common.variant.erasure_shard_offsets()
                    .unwrap_or((SIZE_OF_SIGNATURE, SIZE_OF_SIGNATURE + data_shard_size));
                
                if shred.raw.len() >= end {
                    let mut shard = shred.raw[start..end].to_vec();
                    shard.resize(data_shard_size, 0);
                    shards[idx] = Some(shard);
                    raw_shreds[idx] = Some(shred.raw.clone());
                }
            }
        }
        
        // Fill in coding shreds we have
        for (&pos, shred) in &self.coding_shreds {
            let idx = num_data + pos as usize;
            if idx < total_shards {
                // Extract erasure shard from raw bytes
                let (start, end) = shred.common.variant.erasure_shard_offsets()
                    .unwrap_or((SIZE_OF_CODING_SHRED_HEADERS, SIZE_OF_CODING_SHRED_HEADERS + data_shard_size));
                
                if shred.raw.len() >= end {
                    let mut shard = shred.raw[start..end].to_vec();
                    shard.resize(data_shard_size, 0);
                    shards[idx] = Some(shard);
                }
            }
        }
        
        // Count present shards
        let present_count = shards.iter().filter(|s| s.is_some()).count();
        if present_count < num_data {
            debug!(
                slot = self.id.slot,
                fec_set = self.id.fec_set_index,
                present = present_count,
                need = num_data,
                "not enough shreds for RS reconstruction"
            );
            return None;
        }
        
        // Check if we already have all data shreds (no RS needed)
        let data_complete = raw_shreds.iter().all(|s| s.is_some());
        if data_complete {
            // Extract entry data from raw shreds
            let data_payloads: Vec<Vec<u8>> = raw_shreds.iter()
                .map(|raw| {
                    raw.as_ref().and_then(|r| extract_entry_data(r).map(|d| d.to_vec()))
                        .unwrap_or_default()
                })
                .collect();
            
            return Some(CompleteFecSet {
                id: self.id,
                data_payloads,
                is_last_in_slot: self.is_last_in_slot,
                num_data_shreds: num_data,
            });
        }
        
        // Need actual RS reconstruction
        let rs = match ReedSolomon::new(num_data, num_coding) {
            Ok(rs) => rs,
            Err(e) => {
                warn!(
                    slot = self.id.slot,
                    fec_set = self.id.fec_set_index,
                    num_data = num_data,
                    num_coding = num_coding,
                    error = %e,
                    "failed to create Reed-Solomon decoder"
                );
                return None;
            }
        };
        
        // Perform reconstruction
        match rs.reconstruct(&mut shards) {
            Ok(()) => {
                debug!(
                    slot = self.id.slot,
                    fec_set = self.id.fec_set_index,
                    data_present = self.data_shreds.len(),
                    coding_present = self.coding_shreds.len(),
                    "RS reconstruction successful"
                );
                
                // Extract entry data from reconstructed shards
                // For shreds we already had, use the original raw bytes
                // For reconstructed shreds, we need to rebuild the raw bytes and extract data
                let data_payloads: Vec<Vec<u8>> = (0..num_data).map(|idx| {
                    if let Some(raw) = &raw_shreds[idx] {
                        // We had this shred, extract data from raw
                        extract_entry_data(raw).map(|d| d.to_vec()).unwrap_or_default()
                    } else {
                        // Reconstructed shred - the shard contains bytes [64..88+capacity]
                        // We need to extract entry data from [88..size] relative to original shred
                        // Which is [24..size-64] relative to the shard
                        let shard = shards[idx].as_ref().map(|s| s.as_slice()).unwrap_or(&[]);
                        
                        // The shard contains: [header bytes 64..88 = 24 bytes] + [data 0..capacity]
                        // The size field is at bytes 86-87 in the original shred = bytes 22-23 in shard
                        if shard.len() >= 24 {
                            let size = u16::from_le_bytes([shard[22], shard[23]]) as usize;
                            let data_len = size.saturating_sub(SIZE_OF_DATA_SHRED_HEADERS);
                            
                            if data_len <= capacity && data_len <= shard.len() - 24 {
                                shard[24..24 + data_len].to_vec()
                            } else if data_len == 0 {
                                // Empty shred (padding at end of slot)
                                Vec::new()
                            } else {
                                // Size field is invalid - this is a problem!
                                warn!(
                                    slot = self.id.slot,
                                    fec_set = self.id.fec_set_index,
                                    shred_idx = idx,
                                    size = size,
                                    data_len = data_len,
                                    capacity = capacity,
                                    "RS reconstructed shred has invalid size field!"
                                );
                                // Return empty to avoid corrupting the stream
                                Vec::new()
                            }
                        } else {
                            Vec::new()
                        }
                    }
                }).collect();
                
                Some(CompleteFecSet {
                    id: self.id,
                    data_payloads,
                    is_last_in_slot: self.is_last_in_slot,
                    num_data_shreds: num_data,
                })
            }
            Err(e) => {
                warn!(
                    slot = self.id.slot,
                    fec_set = self.id.fec_set_index,
                    data_present = self.data_shreds.len(),
                    coding_present = self.coding_shreds.len(),
                    error = %e,
                    "RS reconstruction failed"
                );
                None
            }
        }
    }
    
    /// Get age of this FEC set
    pub fn age(&self) -> Duration {
        self.created_at.elapsed()
    }
    
    /// Number of shreds received
    pub fn shred_count(&self) -> usize {
        self.data_shreds.len() + self.coding_shreds.len()
    }
}

// ============================================================================
// FEC Set Assembler
// ============================================================================

/// Configuration for the FEC assembler
#[derive(Debug, Clone)]
pub struct FecAssemblerConfig {
    /// Maximum age before expiring an incomplete FEC set (same as fec_set_ttl)
    pub fec_set_timeout: Duration,
    /// Alias for fec_set_timeout for API compatibility
    pub fec_set_ttl: Duration,
    /// Maximum number of tracked FEC sets (total across all slots)
    pub max_tracked_fec_sets: usize,
    /// Maximum number of tracked FEC sets per slot
    pub max_fec_sets_per_slot: usize,
    /// Maximum number of tracked slots
    pub max_slots: usize,
}

impl Default for FecAssemblerConfig {
    fn default() -> Self {
        Self {
            fec_set_timeout: Duration::from_secs(5),
            fec_set_ttl: Duration::from_secs(5),
            max_tracked_fec_sets: 4096,
            max_fec_sets_per_slot: 1024,
            max_slots: 4,
        }
    }
}

/// Assembles FEC sets from incoming shreds
pub struct FecSetAssembler {
    /// FEC sets being assembled, keyed by (slot, fec_set_index)
    fec_sets: HashMap<ErasureSetId, FecSet>,
    /// Already completed FEC sets (to avoid duplicates from redundant shreds)
    completed_ids: HashSet<ErasureSetId>,
    /// Minimum slot for completed_ids cleanup
    completed_min_slot: u64,
    /// Completed FEC sets ready for entry extraction
    completed_tx: mpsc::Sender<CompleteFecSet>,
    /// Configuration
    config: FecAssemblerConfig,
    /// Statistics
    stats: FecAssemblerStats,
}

#[derive(Debug, Default, Clone)]
pub struct FecAssemblerStats {
    pub shreds_received: u64,
    pub data_shreds_received: u64,
    pub coding_shreds_received: u64,
    pub fec_sets_completed: u64,
    pub fec_sets_expired: u64,
    pub reconstruction_attempts: u64,
    pub reconstruction_successes: u64,
}

impl FecSetAssembler {
    pub fn new(completed_tx: mpsc::Sender<CompleteFecSet>, config: FecAssemblerConfig) -> Self {
        Self {
            fec_sets: HashMap::with_capacity(config.max_slots * config.max_fec_sets_per_slot),
            completed_ids: HashSet::with_capacity(config.max_slots * config.max_fec_sets_per_slot),
            completed_min_slot: 0,
            completed_tx,
            config,
            stats: FecAssemblerStats::default(),
        }
    }
    
    /// Process an incoming shred
    pub async fn process_shred(&mut self, shred: ParsedShred) {
        self.stats.shreds_received += 1;
        
        match shred.shred_type() {
            ShredType::Data => self.stats.data_shreds_received += 1,
            ShredType::Code => self.stats.coding_shreds_received += 1,
        }
        
        let id = ErasureSetId::new(shred.slot(), shred.fec_set_index());
        
        // Skip if we already completed this FEC set (dedup redundant shreds)
        if self.completed_ids.contains(&id) {
            trace!(
                slot = id.slot,
                fec_set = id.fec_set_index,
                "skipping shred for already-completed FEC set"
            );
            return;
        }
        
        // Get or create FEC set
        let fec_set = self.fec_sets
            .entry(id)
            .or_insert_with(|| FecSet::new(id));
        
        // Add shred to set
        let is_complete = fec_set.add_shred(shred);
        
        if is_complete {
            // Try to complete and send
            if let Some(complete) = fec_set.try_reconstruct() {
                self.stats.fec_sets_completed += 1;
                self.stats.reconstruction_attempts += 1;
                self.stats.reconstruction_successes += 1;
                
                debug!(
                    slot = id.slot,
                    fec_set = id.fec_set_index,
                    data_shreds = complete.num_data_shreds,
                    is_last = complete.is_last_in_slot,
                    "FEC set complete"
                );
                
                // Send completed set
                if let Err(e) = self.completed_tx.send(complete).await {
                    warn!("failed to send completed FEC set: {}", e);
                }
                
                // Mark as completed and remove from tracking
                self.completed_ids.insert(id);
                self.fec_sets.remove(&id);
                
                // Update min slot for cleanup
                if id.slot > self.completed_min_slot {
                    self.completed_min_slot = id.slot.saturating_sub(self.config.max_slots as u64);
                    // Clean up old completed IDs
                    self.completed_ids.retain(|cid| cid.slot >= self.completed_min_slot);
                }
            }
        }
        
        // Periodically clean up old FEC sets
        if self.stats.shreds_received % 1000 == 0 {
            self.cleanup_expired();
        }
    }
    
    /// Remove expired FEC sets
    fn cleanup_expired(&mut self) {
        let timeout = self.config.fec_set_timeout;
        let expired: Vec<ErasureSetId> = self.fec_sets
            .iter()
            .filter(|(_, set)| set.age() > timeout)
            .map(|(id, _)| *id)
            .collect();
        
        for id in expired {
            self.fec_sets.remove(&id);
            self.stats.fec_sets_expired += 1;
            trace!(slot = id.slot, fec_set = id.fec_set_index, "expired FEC set");
        }
    }
    
    /// Get current statistics
    pub fn stats(&self) -> &FecAssemblerStats {
        &self.stats
    }
    
    /// Number of FEC sets being tracked
    pub fn tracked_fec_sets(&self) -> usize {
        self.fec_sets.len()
    }
    
    /// Remove all FEC sets for slots older than the given slot
    pub fn prune_old_slots(&mut self, min_slot: u64) {
        self.fec_sets.retain(|id, _| id.slot >= min_slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbine::shred::{ShredCommonHeader, ShredVariant, DataShredHeader};
    
    fn make_test_data_shred(slot: u64, index: u32, fec_set_index: u32, is_last: bool) -> ParsedShred {
        let mut flags = 0u8;
        if is_last {
            flags |= DataShredHeader::FLAG_DATA_COMPLETE_SHRED;
        }
        
        ParsedShred {
            common: ShredCommonHeader {
                signature_offset: 0,
                variant: ShredVariant {
                    shred_type: ShredType::Data,
                    proof_size: 0,
                    resigned: false,
                },
                slot,
                index,
                version: 0,
                fec_set_index,
            },
            inner: ShredInner::Data {
                header: DataShredHeader {
                    parent_offset: 1,
                    flags,
                    size: 100,
                },
                payload: vec![0u8; 100],
            },
            raw: vec![],
        }
    }
    
    #[test]
    fn test_fec_set_completion_with_all_data() {
        let id = ErasureSetId::new(100, 0);
        let mut fec_set = FecSet::new(id);
        
        // Add 32 data shreds
        for i in 0..32 {
            let is_last = i == 31;
            let shred = make_test_data_shred(100, i, 0, is_last);
            fec_set.add_shred(shred);
        }
        
        assert!(fec_set.is_complete());
        
        let complete = fec_set.try_reconstruct().unwrap();
        assert_eq!(complete.num_data_shreds, 32);
    }
    
    #[test]
    fn test_erasure_set_id() {
        let id1 = ErasureSetId::new(100, 0);
        let id2 = ErasureSetId::new(100, 0);
        let id3 = ErasureSetId::new(100, 32);
        
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }
}
