# Solana Architecture Deep Dive

A comprehensive technical reference for understanding Solana's blockchain architecture, transaction model, data propagation, and infrastructure.

---

## Table of Contents

1. [Core Concepts](#core-concepts)
2. [Account Model](#account-model)
3. [Instructions](#instructions)
4. [Transactions](#transactions)
5. [Versioned Transactions](#versioned-transactions)
6. [Shreds - Data Propagation Unit](#shreds---data-propagation-unit)
7. [Turbine Protocol](#turbine-protocol)
8. [RPC Nodes & Infrastructure](#rpc-nodes--infrastructure)
9. [Commitment Levels](#commitment-levels)
10. [Data Streaming Methods](#data-streaming-methods)
11. [Key Terminology](#key-terminology)
12. [References](#references)

---

## Core Concepts

> **Source**: https://solana.com/docs/core

Solana is designed for maximum speed and throughput. To understand Solana development, you need to grasp these fundamental concepts:

### The Solana Model

| Concept | Description |
|---------|-------------|
| **Accounts** | All data on Solana is stored as accounts |
| **Programs** | Smart contracts that define instruction logic |
| **Instructions** | Atomic operations that modify state |
| **Transactions** | Bundles of instructions executed atomically |
| **Clusters** | Networks of validators (Mainnet, Devnet, Testnet) |

### Clusters and Endpoints

| Cluster | Endpoint | Purpose |
|---------|----------|---------|
| Mainnet | `https://api.mainnet-beta.solana.com` | Production |
| Devnet | `https://api.devnet.solana.com` | Developer experimentation |
| Testnet | `https://api.testnet.solana.com` | Validator testing |

> **Source**: https://solana.com/docs/references/clusters

---

## Account Model

> **Source**: https://solana.com/docs/core/accounts

All data on Solana is stored as **Accounts**. The structure resembles a key-value store:

- **Key**: On-chain address (public key)
- **Value**: Account data

### Account Properties

```
┌─────────────────────────────────────────────────────────┐
│ Account                                                 │
├─────────────────────────────────────────────────────────┤
│ lamports: u64        // Balance in lamports             │
│ data: Vec<u8>        // Raw bytes of stored data        │
│ owner: Pubkey        // Program that owns this account  │
│ executable: bool     // Is this a program?              │
│ rent_epoch: u64      // Next epoch rent is due          │
└─────────────────────────────────────────────────────────┘
```

### Account Address Types

1. **Ed25519 Public Key** - Standard keypair-derived address
2. **Program Derived Address (PDA)** - 32-byte value forced off the ed25519 curve
3. **Hash of Ed25519 Key** - With a 32-character string

### Lamports

The smallest unit of SOL:
- **1 SOL = 1,000,000,000 lamports**
- **1 lamport = 0.000000001 SOL**

Micro-lamports are used in prioritization fee calculations:
- **1 lamport = 1,000,000 micro-lamports**

### Rent

Accounts must maintain a minimum balance proportional to their data size to remain **rent-exempt**. All newly created accounts must meet the rent-exemption threshold.

---

## Instructions

> **Source**: https://solana.com/docs/core/instructions

Instructions are the fundamental building blocks for interacting with Solana. Each instruction is a call to a specific program's function.

### Instruction Structure

```rust
pub struct Instruction {
    /// Pubkey of the program that executes this instruction
    pub program_id: Pubkey,
    
    /// Metadata describing accounts to pass to the program
    pub accounts: Vec<AccountMeta>,
    
    /// Opaque data passed to the program for interpretation
    pub data: Vec<u8>,
}
```

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/instruction/src/lib.rs#L94

### Account Metadata

Each account in an instruction has associated metadata:

```rust
pub struct AccountMeta {
    /// Account's public key
    pub pubkey: Pubkey,
    
    /// True if account must sign the transaction
    pub is_signer: bool,
    
    /// True if account data may be mutated
    pub is_writable: bool,
}
```

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/instruction/src/account_meta.rs#L25

### Instruction Components

| Component | Description |
|-----------|-------------|
| `program_id` | Public key of the program to invoke |
| `accounts` | Array of `AccountMeta` for required accounts |
| `data` | Byte array specifying which instruction to invoke + arguments |

---

## Transactions

> **Source**: https://solana.com/docs/core/transactions

Transactions are the mechanism for interacting with the Solana network. A transaction bundles one or more instructions for atomic execution.

### Transaction Structure

```rust
pub struct Transaction {
    /// Array of signatures
    #[serde(with = "short_vec")]
    pub signatures: Vec<Signature>,
    
    /// The transaction message
    pub message: Message,
}
```

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/src/transaction/mod.rs#L207

### Size Constraints

| Constraint | Value | Rationale |
|------------|-------|-----------|
| Max Transaction Size | **1232 bytes** | IPv6 min MTU (1280) - headers (48) |
| IPv6 Header | 40 bytes | Standard IPv6 |
| UDP/Fragment Header | 8 bytes | UDP overhead |

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/packet/src/lib.rs#L29

This limit prevents packet fragmentation on standard internet infrastructure (Ethernet MTU = 1500 bytes).

### Signatures

Each signature is **64 bytes** (Ed25519):
- R component: 32 bytes (packed Edwards point)
- S component: 32 bytes (scalar in range `0 <= S < L`)

The **first signature** is from the fee payer and serves as the **transaction ID**.

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/signature/src/lib.rs#L30

### Message Structure

```rust
pub struct Message {
    /// Message header with signature counts
    pub header: MessageHeader,
    
    /// All account keys used by this transaction
    #[serde(with = "short_vec")]
    pub account_keys: Vec<Pubkey>,
    
    /// Recent blockhash (acts as timestamp)
    pub recent_blockhash: Hash,
    
    /// Instructions to execute atomically
    #[serde(with = "short_vec")]
    pub instructions: Vec<CompiledInstruction>,
}
```

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/program/src/message/legacy.rs#L131

### Message Header

```rust
pub struct MessageHeader {
    /// Total signatures required for validity
    pub num_required_signatures: u8,
    
    /// Number of read-only signed accounts
    pub num_readonly_signed_accounts: u8,
    
    /// Number of read-only unsigned accounts
    pub num_readonly_unsigned_accounts: u8,
}
```

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/program/src/message/mod.rs#L97

### Account Keys Ordering

The `account_keys` array must be **strictly ordered**:

1. **Signer + Writable** (can sign and modify)
2. **Signer + Read-only** (can sign, cannot modify)
3. **Non-signer + Writable** (cannot sign, can modify)
4. **Non-signer + Read-only** (cannot sign or modify)

This ordering allows permissions to be derived from the header without storing them per-account.

### Recent Blockhash

- Acts as a **transaction timestamp**
- Prevents **duplicate transactions**
- **Expires after ~150 blocks** (~1 minute at 400ms/block)
- Obtained via `getLatestBlockhash` RPC method

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/clock/src/lib.rs#L134

### Compiled Instructions

```rust
pub struct CompiledInstruction {
    /// Index into account_keys for program address
    pub program_id_index: u8,
    
    /// Indices into account_keys for instruction accounts
    #[serde(with = "short_vec")]
    pub accounts: Vec<u8>,
    
    /// Program input data
    #[serde(with = "short_vec")]
    pub data: Vec<u8>,
}
```

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/program/src/instruction.rs#L22

### Compact Array Format

Arrays are serialized in compact format:
1. Length encoded as **compact-u16** (variable-length)
2. Items listed sequentially

> **Source**: https://github.com/anza-xyz/agave/blob/v2.1.13/short-vec/src/lib.rs

### Transaction Atomicity

**Critical Property**: Transactions are atomic. If any single instruction fails, the entire transaction fails and no state changes occur.

---

## Versioned Transactions

> **Source**: https://www.quicknode.com/guides/solana-development/transactions/how-to-use-versioned-transactions-on-solana

Since **Epoch 358** (October 10, 2022), Solana supports versioned transactions.

### Transaction Versions

| Version | Description | Features |
|---------|-------------|----------|
| `legacy` | Original transaction format | Standard accounts only |
| `0` (V0) | Versioned transaction | Address Lookup Tables |

### Address Lookup Tables (ALT)

V0 transactions can reference **Address Lookup Tables**, allowing:
- Up to **256 accounts** via a single 32-byte table address
- Dramatically reduced transaction size
- More complex transactions within size limits

### Creating V0 Transactions

```typescript
import {
    TransactionMessage,
    VersionedTransaction
} from '@solana/web3.js';

// Create message with version 0
const messageV0 = new TransactionMessage({
    payerKey: signer.publicKey,
    recentBlockhash: latestBlockhash.blockhash,
    instructions: txInstructions
}).compileToV0Message();

// Create versioned transaction
const transaction = new VersionedTransaction(messageV0);

// Sign BEFORE sending
transaction.sign([signer]);

// Send
const txid = await connection.sendTransaction(transaction);
```

**Important**: V0 transactions must be signed **before** calling `sendTransaction`.

---

## Shreds - Data Propagation Unit

> **Source**: https://github.com/solana-foundation/specs/blob/main/p2p/shred.md
> **Source**: https://www.helius.dev/blog/solana-shreds

Shreds are the **atomic unit of block data propagation** in Solana. Blocks are split into ~1.2KB packets for rapid network transmission.

### Why Shreds?

Solana cannot transmit entire blocks as single units due to:
- Network latency requirements
- Packet size constraints
- Need for parallel processing and FEC recovery

### Shred Size

| Constant | Value | Derivation |
|----------|-------|------------|
| `SHRED_SZ_MAX` | 1228 bytes | IPv6 min MTU (1280) - IPv6 header (40) - UDP (8) - nonce (4) |

### Shred Types

#### Data Shreds
Contain serialized transaction entry data:
- Serialized entry batches
- Transaction payloads
- Metadata flags

#### Coding Shreds
Provide redundancy via **Reed-Solomon erasure coding**:
- Enable recovery from packet loss
- Organized into FEC (Forward Error Correction) sets
- Typically 32 data + 32 coding shreds per FEC set
- Can tolerate up to **50% packet loss**

### Shred Revisions

| Version | Description |
|---------|-------------|
| v1 | Genesis revision - legacy code/data shreds |
| v2 | Added explicit `size` field at offset 0x56 |
| v3 | Introduced Merkle authentication scheme |

### Common Header (83 bytes)

| Offset | Size | Type | Field | Description |
|--------|------|------|-------|-------------|
| 0x00 | 64B | Ed25519 sig | signature | Block producer signature |
| 0x40 | 1B | u8 | variant | Shred type + auth scheme |
| 0x41 | 8B | u64 | slot | Slot number |
| 0x49 | 4B | u32 | shred_index | Index within slot |
| 0x4d | 2B | u16 | shred_version | Network fork identifier |
| 0x4f | 4B | u32 | fec_set_index | First shred index in FEC set |

### Shred Variant Encoding

The variant byte encodes type and authentication:

| High 4 bits | Low 4 bits | Type | Auth |
|-------------|------------|------|------|
| 0x5 | 0xa | Code | Legacy |
| 0xa | 0x5 | Data | Legacy |
| 0x4 | Tree height (h) | Code | Merkle |
| 0x8 | Tree height (h) | Data | Merkle |

### Data Shred Header (v2)

| Offset | Size | Type | Field |
|--------|------|------|-------|
| 0x53 | 2B | u16 | parent_offset (slot distance to parent) |
| 0x55 | 1B | u8 | data_flags |
| 0x56 | 2B | u16 | size |

**Data Flags**:
| Bit | Field | Description |
|-----|-------|-------------|
| 7 | block_complete | Last shred in block |
| 6 | batch_complete | Last shred in entry batch |
| 0:6 | batch_tick | Ticks since slot start |

Payload begins at offset **0x58**.

### Coding Shred Header

| Offset | Size | Type | Field |
|--------|------|------|-------|
| 0x53 | 2B | u16 | num_data_shreds |
| 0x55 | 2B | u16 | num_coding_shreds |
| 0x57 | 2B | u16 | position |

Reed-Solomon payload begins at offset **0x59**.

### Authentication Schemes

#### Legacy Authentication
- Block producer signs each shred individually
- Message: bytes from offset 64 to end (including padding)
- Signature placed in common header

#### Merkle Authentication
- Block producer signs **Merkle root** of entire FEC set
- All shreds in FEC set share the same signature
- Hashes truncated to **20 bytes**

**Merkle Prefixes** (26 bytes, not null-terminated):
- Leaf: `\x00SOLANA_MERKLE_SHREDS_LEAF`
- Interior: `\x01SOLANA_MERKLE_SHREDS_NODE`

### Erasure Coding

FEC sets use Reed-Solomon encoding over **GF(2^8)**:
- Polynomial: `γ^8 + γ^4 + γ^3 + γ^2 + 1`
- Matrix multiplication for encoding
- Allows recovery of missing shreds

**FEC Set Sizes**:
| Data Shreds (N) | Coding Shreds (K) | Total |
|-----------------|-------------------|-------|
| 1 | 17 | 18 |
| 16 | 26 | 42 |
| 32 | 32 | 64 |

For N > 32: K = N (equal ratio)

### Shred Construction Flow

```
1. Block Entry Batching
   └── Group entries, serialize with u64 count prefix

2. Data Shredding
   └── Split into fixed-size pieces (S bytes each)
   └── Legacy: S = 1051 bytes
   └── Merkle: S = 1115 - 20⌈log₂(N+K)⌉ bytes

3. Erasure Coding
   └── Generate K coding shreds from N data shreds
   └── Reed-Solomon encoding

4. Signing
   └── Legacy: Sign each shred
   └── Merkle: Sign Merkle root of FEC set
```

### Shred Reassembly

1. **Verify signatures** for authenticity
2. **Reconstruct missing data shreds** using coding shreds (Reed-Solomon)
3. **Deshred**: Concatenate payloads in order
4. **Deserialize** entry batches into transactions

---

## Turbine Protocol

> **Source**: https://www.helius.dev/blog/solana-shreds

Turbine is Solana's **multi-layer, fanout-based block propagation system**.

### Architecture

```
                    [Leader]
                        │
                        ▼
                   [Root Node]
                   /    |    \
                  /     |     \
        [Layer 1: Up to 200 validators]
              /    |    \
             /     |     \
    [Layer 2: 200 × 200 validators]
                  ...
```

### Key Properties

| Property | Value |
|----------|-------|
| Fanout | 200 nodes per layer |
| Typical hops | 2-3 to reach entire network |
| Propagation time | Milliseconds network-wide |

### Stake-Weighted Shuffling

Validators are sorted by **stake weight**:
- **Higher stake** → Earlier position in tree
- **Earlier position** → Fewer hops from leader
- **Fewer hops** → Lower latency

This creates the **Shred Latency Hierarchy**:

| Validator Type | Latency | Position |
|----------------|---------|----------|
| High-Stake | Lowest | Directly from leader or Layer 1 |
| Medium-Stake | Moderate | Layer 2-3 |
| Unstaked/RPC | Highest | Last in fanout |

### Geographic Considerations

Turbine is **location-agnostic**:
- Physical distance adds latency beyond Turbine hops
- Inter-regional routing (US → Asia) can add significant delays
- Global distribution of validators helps mitigate this

### Future: Rotor

Turbine will be replaced by **Rotor** when Alpenglow is implemented. Stake weighting will still influence broadcast priority.

---

## RPC Nodes & Infrastructure

> **Source**: https://docs.solanalabs.com/operations/setup-an-rpc-node
> **Source**: https://docs.solanalabs.com/operations

### RPC vs Consensus Validator

| Aspect | RPC Node | Consensus Validator |
|--------|----------|---------------------|
| Voting | No (`--no-voting`) | Yes |
| Full API | Yes (`--full-rpc-api`) | Typically limited |
| Purpose | Serve client requests | Secure network |
| Resources | Optimized for queries | Optimized for consensus |

### Key RPC Node Flags

```bash
solana-validator \
  --identity /path/to/validator-keypair.json \
  --known-validator <PUBKEY> \
  --only-known-rpc \
  --full-rpc-api \              # Enable all RPC operations
  --no-voting \                 # Don't participate in consensus
  --private-rpc \               # Don't publish in gossip
  --ledger /mnt/ledger \
  --accounts /mnt/accounts \
  --rpc-port 8899 \
  --rpc-bind-address 0.0.0.0 \
  --limit-ledger-size \         # Limit stored blocks
  --dynamic-port-range 8000-8020 \
  --entrypoint entrypoint.mainnet-beta.solana.com:8001
```

### Account Indexing

Improve query performance with `--account-index`:

| Index | Use Case |
|-------|----------|
| `program-id` | `getProgramAccounts` queries |
| `spl-token-mint` | Token account queries by mint |
| `spl-token-owner` | Token account queries by owner |

### Historical Data: Bigtable

RPC nodes can't store the entire blockchain. For historical data:
- Use `--limit-ledger-size` to control local storage
- Connect to Solana Bigtable for older blocks

> **Source**: https://github.com/solana-labs/solana-bigtable

---

## Commitment Levels

> **Source**: https://solana.com/docs/rpc

Commitment describes how **finalized** a block is at a point in time.

### Levels (Most → Least Finalized)

| Level | Description | Use Case |
|-------|-------------|----------|
| `finalized` | Supermajority confirmed, maximum lockout | Total safety |
| `confirmed` | Voted on by supermajority via gossip/replay | Balanced speed/safety |
| `processed` | Most recent block (may be skipped) | Progress reporting |

### Optimistic Confirmation

Since release 1.3, `confirmed` provides "optimistic confirmation" guarantees.

### Default Behavior

If no commitment specified, nodes default to `finalized`.

> **Source**: https://github.com/anza-xyz/agave/blob/aa0922d6845e119ba466f88497e8209d1c82febc/sdk/src/commitment_config.rs#L199-L203

### Recommendations

- **Dependent transactions**: Use `confirmed`
- **Critical operations**: Use `finalized`
- **Progress tracking**: Use `processed`

---

## Data Streaming Methods

> **Source**: https://www.helius.dev/blog/solana-shreds
> **Source**: https://www.helius.dev/docs/shred-delivery

For latency-critical applications (HFT, liquidations, arbitrage):

### Comparison Table

| Method | Latency | Reliability | Complexity | Processing |
|--------|---------|-------------|------------|------------|
| **Raw Shreds** | Earliest | High (with FEC) | Very High | Custom deshredding |
| **Yellowstone gRPC** | Very Low | High | High | Processed |
| **Enhanced WebSockets** | Low | Medium | Medium | Processed |
| **Standard WebSockets** | Medium | Low | Low | Processed |
| **Webhooks** | Higher | High | Low | Processed |

### Raw Shred Delivery

- Delivered via UDP
- Earliest possible on-chain signal
- **Requires custom deshredding logic**
- Best for: HFT, arbitrage
- Validator stake affects delivery priority

> **Source**: https://www.helius.dev/docs/shred-delivery

### Yellowstone gRPC

- Low-latency validator-to-client streaming
- Access to shred-level data with processing
- Bidirectional streaming
- **Requires dedicated node**
- No native replay capabilities

### WebSockets

Standard subscriptions:
- `accountSubscribe` - Account changes
- `logsSubscribe` - Transaction logs  
- `programSubscribe` - Program events

**Limitations**:
- Data delivered post-confirmation
- Connection can drop (requires retry logic)
- 400ms - 30s latency depending on commitment

### Stake-Weighted Quality of Service (SWQoS)

Higher-staked validators receive:
- Priority in Turbine propagation
- Earlier shred delivery
- Better positioning for latency-sensitive operations

> **Source**: https://www.helius.dev/blog/stake-weighted-quality-of-service-everything-you-need-to-know

---

## Key Terminology

> **Source**: https://solana.com/docs/references/terminology

| Term | Definition |
|------|------------|
| **Account** | Record in ledger holding data or executable code |
| **Block** | Contiguous entries covered by a vote |
| **Blockhash** | Unique hash identifying a block |
| **Cluster** | Set of validators maintaining a ledger |
| **Commitment** | Measure of network confirmation for a block |
| **CPI** | Cross-Program Invocation - one program calling another |
| **Entry** | Tick or transactions entry on the ledger |
| **Epoch** | Time period for which leader schedule is valid |
| **Finality** | When 2/3 of stake has common root |
| **Fork** | Ledger diverged from common entries |
| **Lamport** | 0.000000001 SOL |
| **Leader** | Validator currently appending entries |
| **Lockout** | Duration validator cannot vote on another fork |
| **PDA** | Program Derived Address - no private key |
| **PoH** | Proof of History - cryptographic timestamp |
| **Rent** | Fee for on-chain data storage |
| **Root** | Block at maximum lockout (ancestor of all forks) |
| **Sealevel** | Solana's parallel runtime for programs |
| **Shred** | Smallest unit sent between validators |
| **Slot** | Period for leader to produce a block (~400ms) |
| **Supermajority** | 2/3 of cluster stake |
| **TPU** | Transaction Processing Unit |
| **TVU** | Transaction Validation Unit |
| **VDF** | Verifiable Delay Function |

---

## References

### Official Documentation
- Solana Documentation: https://solana.com/docs
- Core Concepts: https://solana.com/docs/core
- Transactions: https://solana.com/docs/core/transactions
- Instructions: https://solana.com/docs/core/instructions
- Terminology: https://solana.com/docs/references/terminology
- RPC Methods: https://solana.com/docs/rpc

### Specifications
- Shred Specification: https://github.com/solana-foundation/specs/blob/main/p2p/shred.md
- Merkle Tree Spec: https://github.com/solana-foundation/specs/blob/main/core/merkle-tree.md

### Operations
- Validator Operations: https://docs.solanalabs.com/operations
- RPC Node Setup: https://docs.solanalabs.com/operations/setup-an-rpc-node

### Source Code References
- Agave (Validator Client): https://github.com/anza-xyz/agave
- Transaction: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/src/transaction/mod.rs#L207
- Message: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/program/src/message/legacy.rs#L131
- Instruction: https://github.com/anza-xyz/agave/blob/v2.1.13/sdk/instruction/src/lib.rs#L94
- Shred Module: https://github.com/anza-xyz/agave/blob/d22e8e36d84ec4fad88b97cb633b704d1a90e197/ledger/src/shred.rs#L1

### Third-Party Resources
- Helius Shreds Deep Dive: https://www.helius.dev/blog/solana-shreds
- Helius Shred Delivery: https://www.helius.dev/docs/shred-delivery
- Helius Turbine: https://www.helius.dev/blog/turbine-block-propagation-on-solana
- QuickNode Versioned Transactions: https://www.quicknode.com/guides/solana-development/transactions/how-to-use-versioned-transactions-on-solana

---

*Last Updated: December 27, 2025*
