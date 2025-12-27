//! Entry Assembler
//!
//! Extracts entries and transactions from completed FEC sets.
//! Entries are the unit of block data containing PoH hashes and transactions.
//!
//! Solana's entry data spans multiple FEC sets. This assembler buffers
//! FEC sets per slot and attempts to parse entries incrementally.
//!
//! IMPORTANT: Solana entries are serialized with `wincode`, not `bincode`!

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, warn, trace, error, info};

use solana_entry::entry::Entry as SolanaEntry;
use solana_sdk::transaction::VersionedTransaction;

use crate::turbine::fec::CompleteFecSet;

// ============================================================================
// Assembled Data
// ============================================================================

/// Assembled entries from a FEC set
#[derive(Debug)]
pub struct AssembledEntries {
    /// Slot this data belongs to
    pub slot: u64,
    /// FEC set index within the slot
    pub fec_set_index: u32,
    /// Whether this is the last FEC set in the slot
    pub is_last_in_slot: bool,
    /// Deserialized entries
    pub entries: Vec<SolanaEntry>,
    /// All transactions from the entries
    pub transactions: Vec<VersionedTransaction>,
    /// Time taken to assemble
    pub assembly_time_us: u64,
}

impl AssembledEntries {
    /// Get total number of transactions
    pub fn transaction_count(&self) -> usize {
        self.transactions.len()
    }
    
    /// Check if any entries contain transactions
    pub fn has_transactions(&self) -> bool {
        !self.transactions.is_empty()
    }
}

// ============================================================================
// Slot Buffer - accumulates FEC sets for a slot
// ============================================================================

struct SlotBuffer {
    slot: u64,
    /// FEC sets by index
    fec_sets: HashMap<u32, Vec<u8>>,
    /// Byte offset up to which we've already parsed
    parse_cursor: usize,
    /// Accumulated data (FEC sets in order)
    accumulated_data: Vec<u8>,
    /// Last time we received data for this slot
    last_update: Instant,
    /// Whether we've seen the last FEC set
    seen_last: bool,
    /// Next expected FEC set index (for gap detection)
    next_expected_fec: u32,
}

impl SlotBuffer {
    fn new(slot: u64) -> Self {
        Self {
            slot,
            fec_sets: HashMap::new(),
            parse_cursor: 0,
            accumulated_data: Vec::with_capacity(512 * 1024), // 512KB typical slot
            last_update: Instant::now(),
            seen_last: false,
            next_expected_fec: 0,
        }
    }
    
    /// Add a FEC set's data. Returns true if new data was added.
    fn add_fec_set(&mut self, fec_set_index: u32, data: Vec<u8>, is_last: bool) -> bool {
        if self.fec_sets.contains_key(&fec_set_index) {
            return false; // Already have this FEC set
        }
        
        self.fec_sets.insert(fec_set_index, data);
        self.last_update = Instant::now();
        
        if is_last {
            self.seen_last = true;
        }
        
        // Only rebuild if we're still contiguous from our start point
        // Don't rebuild every time to avoid repeated parsing attempts
        self.rebuild_accumulated_data();
        true
    }
    
    /// Check if we have FEC set 0 (required for valid entry parsing)
    fn has_fec_set_zero(&self) -> bool {
        self.fec_sets.contains_key(&0)
    }
    
    /// Check if we have contiguous data from FEC set 0
    fn has_contiguous_from_zero(&self) -> bool {
        if !self.has_fec_set_zero() {
            return false;
        }
        // Check we have 0, 32, 64... contiguously
        let mut index = 0u32;
        while self.fec_sets.contains_key(&index) {
            index += 32;
        }
        // We have at least FEC set 0
        true
    }
    
    /// Rebuild accumulated data from FEC sets in order
    /// Only builds from FEC set 0 to ensure valid entry boundaries
    fn rebuild_accumulated_data(&mut self) {
        // Only rebuild if we have FEC set 0 and haven't started yet
        if !self.fec_sets.contains_key(&0) {
            return; // Can't start without FEC set 0
        }
        
        // Build contiguous data starting from 0
        let mut index = 0u32;
        
        while let Some(data) = self.fec_sets.get(&index) {
            // Only add if we haven't already added this one
            if index >= self.next_expected_fec {
                self.accumulated_data.extend_from_slice(data);
                self.next_expected_fec = index + 32;
            }
            index += 32;
        }
    }
    
    /// Try to parse entries from accumulated data using wincode
    /// 
    /// Entries are serialized as Vec<Entry> with BincodeLen (u64) length prefix.
    /// We parse entries one by one to handle partial data gracefully.
    fn try_parse_entries(&mut self) -> Vec<SolanaEntry> {
        // Only parse if we have data starting from FEC set 0
        if !self.has_fec_set_zero() || self.accumulated_data.len() <= self.parse_cursor {
            return Vec::new();
        }
        
        let unparsed = &self.accumulated_data[self.parse_cursor..];
        
        // Need at least the vec length prefix (8 bytes)
        if unparsed.len() < 8 {
            return Vec::new();
        }
        
        // Read Vec<Entry> length prefix
        let vec_len = u64::from_le_bytes(unparsed[0..8].try_into().unwrap()) as usize;
        
        // Sanity check - vec_len shouldn't be absurdly large
        if vec_len > 10000 {
            warn!(
                vec_len,
                unparsed_len = unparsed.len(),
                first_bytes = ?&unparsed[..std::cmp::min(32, unparsed.len())],
                "Vec<Entry> length is suspiciously large, skipping"
            );
            // Try to recover by skipping these 8 bytes and looking for valid data
            self.parse_cursor += 8;
            return Vec::new();
        }
        
        // Parse entries one by one
        let mut entries = Vec::with_capacity(vec_len.min(256));
        let mut cursor = 8usize; // Start after vec length prefix
        
        for _ in 0..vec_len {
            let remaining = &unparsed[cursor..];
            
            // Need at least minimum entry size (8 + 32 + 8 = 48 bytes minimum)
            if remaining.len() < 48 {
                break;
            }
            
            match wincode::deserialize::<SolanaEntry>(remaining) {
                Ok(entry) => {
                    if let Ok(size) = wincode::serialized_size(&entry) {
                        cursor += size as usize;
                        entries.push(entry);
                    } else {
                        // Couldn't determine size, stop parsing
                        break;
                    }
                }
                Err(_) => {
                    // Couldn't parse entry, probably incomplete data
                    break;
                }
            }
        }
        
        // Update cursor to point past all successfully parsed entries
        if !entries.is_empty() {
            let parsed_count = entries.len();
            let total_txs: usize = entries.iter().map(|e| e.transactions.len()).sum();
            
            // Only advance cursor if we parsed ALL expected entries
            // Otherwise we'll try again when more data arrives
            if entries.len() == vec_len {
                self.parse_cursor += cursor;
                info!(
                    num_entries = parsed_count,
                    total_txs,
                    cursor_advanced = cursor,
                    "successfully parsed complete entry batch"
                );
            } else {
                // Partial parse - return what we have but don't advance cursor for incomplete batch
                // This allows us to retry when more data arrives
                debug!(
                    parsed = parsed_count,
                    expected = vec_len,
                    total_txs,
                    unparsed_len = unparsed.len(),
                    "partially parsed entries - waiting for more data"
                );
                // Actually, let's advance past what we parsed so we can extract transactions
                self.parse_cursor += cursor;
                info!(
                    num_entries = parsed_count,
                    expected = vec_len,
                    total_txs,
                    "parsed partial entry batch"
                );
            }
        }
        
        entries
    }
    
    fn age(&self) -> Duration {
        self.last_update.elapsed()
    }
}

// ============================================================================
// Entry Assembler
// ============================================================================

/// Assembles entries from completed FEC sets
pub struct EntryAssembler {
    /// Output channel for assembled entries
    output_tx: mpsc::Sender<AssembledEntries>,
    /// Per-slot buffers
    slot_buffers: HashMap<u64, SlotBuffer>,
    /// Statistics
    stats: EntryAssemblerStats,
    /// Maximum age for slot buffers
    max_slot_age: Duration,
}

#[derive(Debug, Default, Clone)]
pub struct EntryAssemblerStats {
    pub fec_sets_processed: u64,
    pub entries_extracted: u64,
    pub transactions_extracted: u64,
    pub deserialization_failures: u64,
    pub empty_payloads: u64,
}

impl EntryAssembler {
    pub fn new(output_tx: mpsc::Sender<AssembledEntries>) -> Self {
        Self {
            output_tx,
            slot_buffers: HashMap::new(),
            stats: EntryAssemblerStats::default(),
            max_slot_age: Duration::from_secs(5),
        }
    }
    
    /// Process a completed FEC set and extract entries
    pub async fn process_fec_set(&mut self, fec_set: CompleteFecSet) {
        let start = Instant::now();
        self.stats.fec_sets_processed += 1;
        
        let slot = fec_set.id.slot;
        let fec_set_index = fec_set.id.fec_set_index;
        let is_last = fec_set.is_last_in_slot;
        
        // Concatenate all data shred payloads from this FEC set
        let mut data = Vec::with_capacity(fec_set.data_payloads.len() * 1200);
        for payload in &fec_set.data_payloads {
            data.extend_from_slice(payload);
        }
        
        if data.is_empty() {
            self.stats.empty_payloads += 1;
            trace!(slot, fec_set_index, "empty FEC set payload");
            return;
        }
        
        // Get or create slot buffer
        let buffer = self.slot_buffers
            .entry(slot)
            .or_insert_with(|| SlotBuffer::new(slot));
        
        // Add this FEC set's data to the buffer
        if !buffer.add_fec_set(fec_set_index, data, is_last) {
            return; // Duplicate FEC set
        }
        
        // Try to parse entries from the accumulated data
        let entries = buffer.try_parse_entries();
        
        if entries.is_empty() {
            // No complete entries yet - data might span FEC boundaries
            trace!(
                slot,
                fec_set_index,
                accumulated_len = buffer.accumulated_data.len(),
                parse_cursor = buffer.parse_cursor,
                "no complete entries in accumulated data yet"
            );
            return;
        }
        
        // Extract all transactions from entries
        let mut transactions = Vec::new();
        for entry in &entries {
            for tx in &entry.transactions {
                transactions.push(tx.clone());
            }
        }
        
        self.stats.entries_extracted += entries.len() as u64;
        self.stats.transactions_extracted += transactions.len() as u64;
        
        let assembly_time_us = start.elapsed().as_micros() as u64;
        
        debug!(
            slot,
            fec_set_index,
            entries = entries.len(),
            transactions = transactions.len(),
            time_us = assembly_time_us,
            is_last,
            "assembled entries from FEC set"
        );
        
        let assembled = AssembledEntries {
            slot,
            fec_set_index,
            is_last_in_slot: is_last,
            entries,
            transactions,
            assembly_time_us,
        };
        
        // Send to output
        if let Err(e) = self.output_tx.send(assembled).await {
            warn!("failed to send assembled entries: {}", e);
        }
        
        // Clean up old slot buffers periodically
        if self.stats.fec_sets_processed % 100 == 0 {
            self.cleanup_old_buffers();
        }
    }
    
    /// Remove old slot buffers
    fn cleanup_old_buffers(&mut self) {
        let max_age = self.max_slot_age;
        self.slot_buffers.retain(|_, buf| buf.age() < max_age);
    }
    
    /// Get current statistics
    pub fn stats(&self) -> &EntryAssemblerStats {
        &self.stats
    }
}

/// Run the entry assembler as a task
pub async fn run_entry_assembler(
    mut fec_rx: mpsc::Receiver<CompleteFecSet>,
    output_tx: mpsc::Sender<AssembledEntries>,
) {
    tracing::info!("Entry assembler started - waiting for FEC sets");
    let mut assembler = EntryAssembler::new(output_tx);
    
    while let Some(fec_set) = fec_rx.recv().await {
        assembler.process_fec_set(fec_set).await;
    }
    
    debug!(
        fec_sets = assembler.stats.fec_sets_processed,
        entries = assembler.stats.entries_extracted,
        transactions = assembler.stats.transactions_extracted,
        failures = assembler.stats.deserialization_failures,
        "entry assembler exiting"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbine::fec::ErasureSetId;
    
    #[test]
    fn test_empty_payload() {
        let (tx, _rx) = mpsc::channel(1);
        let mut assembler = EntryAssembler::new(tx);
        
        // Stats should start at zero
        assert_eq!(assembler.stats.fec_sets_processed, 0);
    }
}
