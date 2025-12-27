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
    
    /// Get diagnostic info for logging
    fn diagnostic_info(&self) -> (usize, usize, bool, u32, usize) {
        (
            self.accumulated_data.len(),
            self.parse_cursor,
            self.has_fec_set_zero(),
            self.next_expected_fec,
            self.fec_sets.len(),
        )
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
    /// Entry data in shreds is serialized as a Vec<Entry> with wincode.
    /// The format is: [8-byte length prefix] [Entry 0] [Entry 1] ... [Entry N-1]
    /// We must deserialize the entire Vec at once - cannot stream individual entries.
    /// 
    /// We wait until seen_last is true (DATA_COMPLETE_SHRED received) to ensure
    /// we have the complete serialized Vec<Entry> before attempting deserialization.
    fn try_parse_entries(&mut self) -> Vec<SolanaEntry> {
        // Only parse if we have data starting from FEC set 0
        if !self.has_fec_set_zero() || self.accumulated_data.is_empty() {
            return Vec::new();
        }
        
        // Already parsed this slot
        if self.parse_cursor >= self.accumulated_data.len() {
            return Vec::new();
        }
        
        // Wait for slot completion - the serialized data is a Vec<Entry> and we need
        // the complete Vec to deserialize. Attempting partial deserialization will fail
        // because wincode reads the length prefix and expects all entries to be present.
        if !self.seen_last {
            return Vec::new();
        }
        
        // Deserialize the complete Vec<Entry> from accumulated shred data
        match wincode::deserialize::<Vec<SolanaEntry>>(&self.accumulated_data) {
            Ok(entries) => {
                let total_txs: usize = entries.iter().map(|e| e.transactions.len()).sum();
                
                // Mark as fully parsed
                self.parse_cursor = self.accumulated_data.len();
                
                info!(
                    slot = self.slot,
                    num_entries = entries.len(),
                    total_txs,
                    data_len = self.accumulated_data.len(),
                    "successfully parsed entries from complete slot"
                );
                
                entries
            }
            Err(e) => {
                // Log error with diagnostic info
                let preview: Vec<u8> = self.accumulated_data.iter().take(64).cloned().collect();
                let vec_len = if self.accumulated_data.len() >= 8 {
                    u64::from_le_bytes(self.accumulated_data[0..8].try_into().unwrap_or([0u8; 8]))
                } else { 0 };
                
                warn!(
                    slot = self.slot,
                    data_len = self.accumulated_data.len(),
                    vec_length_prefix = vec_len,
                    error = %e,
                    first_64_bytes = ?preview,
                    fec_sets_count = self.fec_sets.len(),
                    "failed to deserialize Vec<Entry> from complete slot"
                );
                
                // Mark as attempted to avoid repeated failures
                self.parse_cursor = self.accumulated_data.len();
                Vec::new()
            }
        }
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
        let mut non_empty_payloads = 0usize;
        for payload in &fec_set.data_payloads {
            if !payload.is_empty() {
                non_empty_payloads += 1;
            }
            data.extend_from_slice(payload);
        }
        
        // Log every 100th FEC set to track data flow
        if self.stats.fec_sets_processed % 100 == 1 {
            debug!(
                slot,
                fec_set_index,
                num_payloads = fec_set.data_payloads.len(),
                non_empty_payloads,
                total_bytes = data.len(),
                is_last,
                "processing FEC set"
            );
        }
        
        if data.is_empty() {
            self.stats.empty_payloads += 1;
            debug!(slot, fec_set_index, "empty FEC set payload - all {} payloads were empty", fec_set.data_payloads.len());
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
            // Log more frequently to diagnose the issue
            if self.stats.fec_sets_processed % 50 == 1 {
                let (acc_len, cursor, has_zero, next_exp, fec_count) = buffer.diagnostic_info();
                debug!(
                    slot,
                    fec_set_index,
                    accumulated_len = acc_len,
                    parse_cursor = cursor,
                    has_fec_zero = has_zero,
                    next_expected = next_exp,
                    fec_sets_count = fec_count,
                    "no entries parsed from accumulated data"
                );
            }
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
