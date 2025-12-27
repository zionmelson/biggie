# Arbitrage Strategy Architecture

## Overview

This bot executes arbitrage on Solana using **cross-pool arbitrage** with ultra-low-latency 
state tracking via Turbine UDP and Geyser.

### Critical Understanding: What Our Data Sources Provide

**Shreds are POST-EXECUTION data.** When you receive a transaction via Turbine or ShredStream,
that transaction has **already been executed and included in a block** by the leader. You cannot
"follow" it in the same block - you can only respond in subsequent blocks.

| Data Source | Timing | What It Contains | Arb Use |
|-------------|--------|------------------|---------|
| Turbine UDP | ~0.5-2ms after block produced | Executed transactions | **Fast state updates** |
| ShredStream | ~5-10ms after block produced | Executed transactions | State updates (fallback) |
| Geyser | ~5-15ms after execution | Confirmed account state | **Ground truth + arb trigger** |

### Viable Strategies

| Strategy | Requires | This Bot |
|----------|----------|----------|
| **Cross-Pool Arbitrage** | Price divergence between pools | ✅ Primary |
| **Post-Swap Price Capture** | See swap → arb resulting divergence | ✅ Supported |
| **Same-Block Following** | Mempool access / Jito pending TX stream | ❌ Not available |
| **Sandwiching** | Mempool access | ❌ Not available |

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         CROSS-POOL ARBITRAGE BOT                            │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│                        ┌─────────────────────┐                              │
│                        │    POOL STATE       │                              │
│                        │    MANAGER          │                              │
│                        │                     │                              │
│                        │  Pool A: reserves   │                              │
│                        │  Pool B: reserves   │                              │
│                        │  Pool C: reserves   │                              │
│                        │  ...                │                              │
│                        │                     │                              │
│                        │  + Price Index      │                              │
│                        │  (pair → pools)     │                              │
│                        └─────────────────────┘                              │
│                           ▲             ▲                                   │
│                           │             │                                   │
│              ┌────────────┴──┐       ┌──┴────────────┐                      │
│              │  FAST PATH    │       │  RELIABLE     │                      │
│              │  (Turbine)    │       │  PATH         │                      │
│              │  ~0.5-2ms     │       │  (Geyser)     │                      │
│              │               │       │  ~5-15ms      │                      │
│              │  Parse swaps  │       │  Vault        │                      │
│              │  from shreds  │       │  balances     │                      │
│              │  → simulate   │       │  → direct     │                      │
│              │  new reserves │       │  state        │                      │
│              └───────────────┘       └───────────────┘                      │
│                     │                       │                               │
│                     └───────┬───────────────┘                               │
│                             │                                               │
│                             ▼ ON ANY STATE CHANGE                           │
│              ┌─────────────────────────────────────┐                        │
│              │      ARBITRAGE EVALUATOR            │                        │
│              │                                     │                        │
│              │  For updated pair (base, quote):    │                        │
│              │  1. Get all pools for this pair     │                        │
│              │  2. Find cheapest buy pool          │                        │
│              │  3. Find best sell pool             │                        │
│              │  4. Calculate spread vs costs       │                        │
│              │  5. If profitable → build TX        │                        │
│              └─────────────────────────────────────┘                        │
│                             │                                               │
│                             ▼ IF PROFITABLE                                 │
│              ┌─────────────────────────────────────┐                        │
│              │      ARB TRANSACTION BUILDER        │                        │
│              │                                     │                        │
│              │  Single atomic TX:                  │                        │
│              │  1. Flash loan borrow               │                        │
│              │  2. Swap on cheap pool              │                        │
│              │  3. Swap on expensive pool          │                        │
│              │  4. Flash loan repay                │                        │
│              │  5. Transfer profit                 │                        │
│              └─────────────────────────────────────┘                        │
│                             │                                               │
│                             ▼                                               │
│              ┌─────────────────────────────────────┐                        │
│              │      JITO SUBMISSION                │                        │
│              │                                     │                        │
│              │  • Single-TX bundle (NOT bundled    │                        │
│              │    with trigger - already executed!) │                        │
│              │  • Competitive tip for priority     │                        │
│              │  • Multi-region for redundancy      │                        │
│              └─────────────────────────────────────┘                        │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## How Each Component Works

### Turbine UDP: Fast State Updates

Turbine provides the **fastest** way to learn about state changes:

```
Block N produced by leader
         │
         ▼ (~0.5-2ms)
┌─────────────────────────────────────────────┐
│            TURBINE RECEIVER                 │
│                                             │
│  1. Receive shreds via UDP                  │
│  2. FEC decode → assemble entries           │
│  3. Parse transactions for DEX swaps        │
│  4. For each swap found:                    │
│     a. Identify pool + direction + amount   │
│     b. Simulate impact on reserves          │
│     c. Update Pool State immediately        │
│     d. TRIGGER arb evaluation               │
└─────────────────────────────────────────────┘
```

**Why Turbine is fast but not authoritative:**
- We're parsing TX data and simulating the result
- Edge cases (failed TXs, complex multi-hop) may be parsed incorrectly
- Geyser provides the true vault balances

### Geyser: Ground Truth State

Geyser provides **authoritative** state updates:

```
┌─────────────────────────────────────────────┐
│              GEYSER CLIENT                  │
│                                             │
│  VAULT SUBSCRIPTIONS:                       │
│  • Subscribe to token vault accounts        │
│  • Receive balance updates directly         │
│  • Update Pool State authoritatively        │
│  • TRIGGER arb evaluation                   │
│                                             │
│  SLOT/BLOCKHASH:                            │
│  • Track current slot for freshness         │
│  • Get blockhash for TX building            │
│                                             │
│  PROGRAM SUBSCRIPTIONS:                     │
│  • Monitor pool accounts for CLMM state     │
│  • Track tick/bin changes                   │
└─────────────────────────────────────────────┘
```

**Why we need both Turbine AND Geyser:**

| Turbine | Geyser |
|---------|--------|
| ⚡ Faster (~0.5-2ms) | ✅ Authoritative (direct state) |
| ⚠️ Simulated from TX data | ✅ Actual vault balances |
| ⚠️ Can miss edge cases | ✅ Never misses state changes |
| 🎯 First-mover advantage | 🎯 Catches what Turbine missed |

---

## The Arbitrage Flow

### Timeline: What Actually Happens

```
TIME    BLOCK   EVENT                           
─────────────────────────────────────────────────────────────────
t=0     N       Large swap executes on Pool A   
                (Leader produces block - we have no visibility yet)

t=1ms   N       Turbine: Shreds arrive          
                → Parse swap: "User sold 1000 SOL on Pool A"
                → Simulate: Pool A reserves changed
                → Update Pool A state
                
t=2ms   N       Arb evaluation triggered:
                "Pool A SOL cheaper than Pool B!"
                → Calculate optimal size
                → Profit > costs? YES
                
t=3ms   N       Build arb TX:
                → Flash loan USDC
                → Buy SOL on Pool A (cheap)
                → Sell SOL on Pool B (expensive)  
                → Repay flash loan
                → Keep profit
                
t=4ms   N       Submit to Jito (single-TX bundle with tip)
                
t=10ms  N       Geyser: Confirms Pool A state
                → Verify our state was correct
                → (If different, would trigger new eval)

t=400ms N+1     Our arb TX included in next block
                (if we won the Jito auction)

t=401ms N+1     Profit realized 🎯
```

### Why Speed Matters (Even Without Mempool Access)

Even though we can't frontrun/follow within the same block:

1. **First to see price divergence** → Submit arb TX before competitors
2. **More accurate state** → Better sizing, fewer reverts  
3. **React to large swaps** → Capture price impact before others

The 5-8ms advantage of Turbine over ShredStream can mean winning vs losing the Jito auction.

---

## Arbitrage Strategies

### Cross-Pool Arbitrage (Primary) ✅

The core profitable strategy:

```
Pool A (Raydium): SOL/USDC @ $99.50  (1 SOL = 99.50 USDC)
Pool B (Orca):    SOL/USDC @ $100.00 (1 SOL = 100.00 USDC)
Divergence:       $0.50 (0.5%)

Arb TX (atomic, single transaction):
  1. Flash loan 1000 USDC from Solend
  2. Buy 10.05 SOL on Raydium (1000 USDC → 10.05 SOL)
  3. Sell 10.05 SOL on Orca (10.05 SOL → 1005 USDC)
  4. Repay 1003 USDC (principal + 0.3% fee)
  5. Profit: 2 USDC (minus gas + tip)
```

**When opportunities arise:**
- Large swap moves one pool's price
- New liquidity added to a pool
- Market volatility creates temporary divergence
- Different pools have different fee tiers

### Three-Hop Arbitrage ✅

For meme tokens paired with different anchors:

```
Pool A: BONK/SOL
Pool B: BONK/USDC
Pool C: SOL/USDC

Arb route:
  1. Flash loan SOL
  2. SOL → BONK on Pool A
  3. BONK → USDC on Pool B  
  4. USDC → SOL on Pool C
  5. Repay flash loan, keep profit
```

### What We DON'T Do

**Same-Block Following** ❌
- Would require seeing trigger TX in mempool before execution
- We only see TXs after they're in a block
- Jito's pending TX stream could enable this (future enhancement)

**Sandwiching** ❌
- Requires frontrunning (mempool access)
- Not possible with our infrastructure

---

## Supported DEXes

| DEX              | Program ID                                     | Pool Type |
| ---------------- | ---------------------------------------------- | --------- |
| Raydium AMM      | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` | Constant Product |
| Raydium CLMM     | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` | Concentrated Liquidity |
| Orca Whirlpool   | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc`  | Concentrated Liquidity |
| Meteora DLMM     | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`  | Dynamic Liquidity |
| Meteora DAMM v2  | `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG`  | Constant Product |
| PancakeSwap CLMM | `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq` | Concentrated Liquidity |

---

## Flash Loan Integration

We use Solend flash loans for capital-free arbitrage:

| Component     | Value                                          |
| ------------- | ---------------------------------------------- |
| Program ID    | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo`  |
| Borrow IX     | `FlashBorrowReserveLiquidity` (instruction 19) |
| Repay IX      | `FlashRepayReserveLiquidity` (instruction 20)  |
| Fee           | 0.3% (30 basis points)                         |

**Atomic execution:** Borrow and repay happen in the same TX. If the arb fails 
to generate enough to repay, the entire TX reverts - no capital at risk.

---

## Configuration

```json
{
  "turbine": {
    "enabled": true,
    "bind_addr": "0.0.0.0:8002",
    "receiver_threads": 2
  },
  "geyser": {
    "endpoint": "http://localhost:10000",
    "skip_votes": true
  },
  "arb": {
    "min_profit_lamports": 50000,
    "min_divergence_bps": 10,
    "tip_profit_share_bps": 5000
  },
  "jito": {
    "endpoint": "https://ny.mainnet.block-engine.jito.wtf",
    "regions": ["ny", "amsterdam", "tokyo"]
  }
}
```

---

## Module Structure

```
src/
├── geyser.rs                # Geyser gRPC client (ground truth state)
├── pool_state.rs            # Pool state manager (single source of truth)
├── pool_discovery.rs        # Pool parsing from on-chain data
├── tip_monitor.rs           # Jito tip floor monitoring
├── multi_region.rs          # Multi-region bundle submission
│
├── shredstream/
│   ├── unified_pipeline.rs  # Main pipeline orchestration
│   ├── parser.rs            # Transaction → ParsedSwap
│   ├── evaluator.rs         # State change → ArbOpportunity
│   ├── arb_builder.rs       # Build atomic arb transactions
│   └── programs.rs          # DEX program IDs
│
├── turbine/                 # Direct UDP shred receiver
│   ├── receiver.rs          # UDP packet handling
│   ├── shred.rs             # Shred parsing
│   ├── fec.rs               # Reed-Solomon FEC decoding
│   ├── assembler.rs         # Entry reconstruction
│   └── integration.rs       # Pipeline integration
│
├── executor.rs              # Transaction building utilities
└── jito.rs                  # Bundle submission
```

---

## Pool State Manager

The `PoolStateManager` is the single source of truth for all pool reserves:

```
┌─────────────────────────────────────────────────────────────────┐
│                    PoolStateManager                             │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  POOL STATE:                                                    │
│  HashMap<Pubkey, PoolState> {                                   │
│      pool_address → {                                           │
│          base_reserve: u64,                                     │
│          quote_reserve: u64,                                    │
│          last_update: Instant,                                  │
│          source: Turbine | Geyser,                              │
│      }                                                          │
│  }                                                              │
│                                                                 │
│  PAIR INDEX (for finding arb routes):                           │
│  HashMap<(Pubkey, Pubkey), Vec<Pubkey>> {                       │
│      (base_mint, quote_mint) → [pool1, pool2, pool3, ...]       │
│  }                                                              │
│                                                                 │
│  VAULT INDEX (for Geyser updates):                              │
│  HashMap<Pubkey, (Pubkey, bool)> {                              │
│      vault_address → (pool_address, is_base_vault)              │
│  }                                                              │
│                                                                 │
├─────────────────────────────────────────────────────────────────┤
│  METHODS:                                                       │
│                                                                 │
│  on_turbine_swap(pool, swap) → Option<ArbOpportunity>           │
│      Simulate swap impact, update state, evaluate arb           │
│                                                                 │
│  on_geyser_vault_update(vault, balance) → Option<ArbOpportunity>│
│      Update reserves directly, evaluate arb                     │
│                                                                 │
│  evaluate_arb_for_pair(base, quote) → Option<ArbOpportunity>    │
│      Compare prices across all pools for this pair              │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## Pipeline Statistics

```
TURBINE[pkts=X shreds=X entries=X swaps=X] GEYSER[vaults=X slots=X] ARB[scans=X opps=X submitted=X won=X] profit=X.XX SOL
```

| Stat | Description |
|------|-------------|
| `turbine_pkts` | UDP packets received |
| `turbine_swaps` | Swaps parsed from shreds |
| `geyser_vaults` | Vault balance updates |
| `arb_scans` | Arb evaluations triggered |
| `arb_opps` | Profitable opportunities found |
| `submitted` | Bundles submitted to Jito |
| `won` | Bundles successfully landed |
| `profit` | Total profit in SOL |

---

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Stale state (arb gone) | Submit fast, Jito simulation catches reverts |
| Competing searchers | Faster state updates (Turbine), competitive tips |
| Flash loan unavailable | Skip opportunity, try different anchor |
| TX simulation fails | Jito rejects upfront, no wasted gas |
| Network latency | Local validator, multi-region submission |
| Pool math errors | Conservative sizing, CLMM tick simulation |

---

## System Requirements

```bash
# Increase UDP buffer for Turbine (required)
sudo sysctl -w net.core.rmem_max=134217728
sudo sysctl -w net.core.rmem_default=134217728

# Recommended: make persistent
echo "net.core.rmem_max=134217728" >> /etc/sysctl.conf
echo "net.core.rmem_default=134217728" >> /etc/sysctl.conf
```

---

## Running

```bash
# Build release binary
cargo build --release

# Run with config
./target/release/tre-arb-bot -c config.json

# Logs written to /tmp/tre-arb.log
```

---

## Future Enhancements

### Same-Block Following (Requires Jito Searcher Access)
If we integrate with Jito's pending transaction stream:
1. Subscribe to `SubscribePendingTransactions`
2. See trigger TXs before they're included
3. Bundle `[trigger_tx, arb_tx]` for same-block execution
4. True followning becomes possible

### Liquidation Arbitrage
Monitor lending protocols for underwater positions:
1. Subscribe to lending protocol accounts via Geyser
2. Calculate health factors in real-time
3. Submit liquidation TX when position becomes unhealthy

---

*Last Updated: December 27, 2025*
