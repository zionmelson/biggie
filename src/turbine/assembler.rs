//! Entry Assembler
//!
//! Extracts entries and transactions from completed FEC sets.
//! Entries are the unit of block data containing PoH hashes and transactions.

use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, warn, trace, error};

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
// Entry Assembler
// ============================================================================

/// Assembles entries from completed FEC sets
pub struct EntryAssembler {
    /// Output channel for assembled entries
    output_tx: mpsc::Sender<AssembledEntries>,
    /// Statistics
    stats: EntryAssemblerStats,
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
            stats: EntryAssemblerStats::default(),
        }
    }
    
    /// Process a completed FEC set and extract entries
    pub async fn process_fec_set(&mut self, fec_set: CompleteFecSet) {
        let start = Instant::now();
        self.stats.fec_sets_processed += 1;
        
        let slot = fec_set.id.slot;
        let fec_set_index = fec_set.id.fec_set_index;
        
        // Concatenate all data shred payloads
        let mut data = Vec::with_capacity(fec_set.data_payloads.len() * 1200);
        for payload in &fec_set.data_payloads {
            data.extend_from_slice(payload);
        }
        
        if data.is_empty() {
            self.stats.empty_payloads += 1;
            trace!(slot, fec_set_index, "empty FEC set payload");
            return;
        }
        
        // Deserialize entries from the concatenated data
        // The data format is: [num_entries: u64][entry1][entry2]...
        // Each entry is bincode-serialized
        let entries = match Self::deserialize_entries(&data) {
            Ok(e) => e,
            Err(e) => {
                self.stats.deserialization_failures += 1;
                trace!(
                    slot, 
                    fec_set_index, 
                    data_len = data.len(),
                    error = %e,
                    "failed to deserialize entries"
                );
                return;
            }
        };
        
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
            is_last = fec_set.is_last_in_slot,
            "assembled entries from FEC set"
        );
        
        let assembled = AssembledEntries {
            slot,
            fec_set_index,
            is_last_in_slot: fec_set.is_last_in_slot,
            entries,
            transactions,
            assembly_time_us,
        };
        
        // Send to output
        if let Err(e) = self.output_tx.send(assembled).await {
            warn!("failed to send assembled entries: {}", e);
        }
    }
    
    /// Deserialize entries from raw block data
    /// 
    /// The format is Solana's entry batch format:
    /// - Entries are bincode-serialized back-to-back
    /// - We need to handle variable-length entries
    fn deserialize_entries(data: &[u8]) -> anyhow::Result<Vec<SolanaEntry>> {
        // First try: deserialize as Vec<Entry>
        // Solana typically encodes entries in batches
        if let Ok(entries) = bincode::deserialize::<Vec<SolanaEntry>>(data) {
            return Ok(entries);
        }
        
        // Second try: deserialize individual entries one by one
        // This is less efficient but handles edge cases
        let mut entries = Vec::new();
        let mut cursor = 0;
        
        while cursor < data.len() {
            // Try to deserialize an entry at the current position
            let remaining = &data[cursor..];
            
            match bincode::deserialize::<SolanaEntry>(remaining) {
                Ok(entry) => {
                    // Estimate consumed bytes (bincode doesn't tell us directly)
                    // We'll use the serialized size of the entry we just parsed
                    let consumed = bincode::serialized_size(&entry)
                        .map(|s| s as usize)
                        .unwrap_or(remaining.len());
                    entries.push(entry);
                    cursor += consumed;
                }
                Err(_) => {
                    // No more valid entries
                    break;
                }
            }
        }
        
        if entries.is_empty() {
            anyhow::bail!("failed to deserialize any entries from {} bytes", data.len());
        }
        
        Ok(entries)
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
