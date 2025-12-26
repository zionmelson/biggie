# MEV Strategy Architecture

## Overview

This bot extracts MEV from Solana using a **unified ShredStream + Geyser architecture**. ShredStream provides early transaction detection before confirmation, while Geyser provides ground truth state confirmation. Both work together via the `PredictiveStateManager` for optimal MEV extraction with accurate state prediction.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                       UNIFIED MEV PIPELINE                              │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  SHREDSTREAM TASK                    GEYSER TASK                        │
│  ═══════════════                     ══════════                         │
│  • Subscribe entries                 • Subscribe transactions           │
│  • Parse transactions                • Subscribe accounts (vaults)      │
│  • Detect swaps EARLY                • Subscribe slots                  │
│         │                                   │                           │
│         ▼                                   ▼                           │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │              PREDICTIVE STATE MANAGER (shared)                   │   │
│  │  • Adds pending swaps ◄── ShredStream                           │   │
│  │  • Confirms/removes pending ◄── Geyser TX updates               │   │
│  │  • Updates confirmed state ◄── Geyser account updates           │   │
│  │  • Computes predicted state = confirmed + pending               │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                              │                                          │
│                              ▼                                          │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │              MEV EVALUATOR (uses predicted state)                │   │
│  │  • Evaluates opportunities against PREDICTED state              │   │
│  │  • Simulates victim impact (already in prediction)              │   │
│  │  • Finds cross-pool arb routes                                  │   │
│  │  • Calculates profit with state prediction                      │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                              │                                          │
│                              ▼                                          │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │              BUNDLE BUILDER & JITO SUBMISSION                    │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

## Key Design Principles

1. **Turbine UDP provides ultra-low-latency TX detection** - Receives shreds directly from validator (~0.5-2ms)
2. **Geyser provides ground truth** - Confirms state after execution
3. **Predictive state bridges the gap** - Simulates pending swaps for accurate evaluation
4. **ShredStream as fallback** - Can be used if Turbine is unavailable (~5-10ms)

## Timing Advantage

With direct Turbine UDP, we receive transactions with minimal latency:

```
Transaction lifecycle (Turbine UDP - Primary):
├── t=0ms      TX enters validator mempool / included in block
├── t=0.5-1ms  Turbine: Shreds received via UDP → entry assembled
├── t=0.5-1ms  Swap detected → PredictiveStateManager updated
├── t=1-2ms    PredictedState recalculated (includes victim's impact)
├── t=1-3ms    MEV Evaluator uses PredictedState → finds opportunity
├── t=2-5ms    Bundle built and submitted to Jito
├── t=5-15ms   Geyser: Victim TX confirmed → PendingSwap removed
├── t=5-15ms   ConfirmedState updated → PredictedState recalculated
├── t=400ms    Block finalized
└── t=500ms    Confirmation propagates
```

### Direct Turbine UDP vs ShredStream Proxy

```
Direct Turbine UDP: Validator TVU → UDP → Bot (~0.5-2ms) ✅ DEFAULT
ShredStream Proxy:  Validator → Jito Relay → Proxy → gRPC → Bot (~5-10ms)
```

This 5-8ms advantage is critical for winning MEV auctions. The bot now uses Turbine UDP by default.

### System Requirements for Turbine UDP

```bash
# Increase UDP buffer size (required for reliable shred reception)
sudo sysctl -w net.core.rmem_max=134217728
sudo sysctl -w net.core.rmem_default=134217728
```

## Supported DEXes

| DEX              | Program ID                                     | Source                                                                                            |
| ---------------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| Raydium AMM      | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` | [program/src/lib.rs](https://github.com/raydium-io/raydium-amm)                                   |
| Raydium CLMM     | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` | [programs/amm/src/lib.rs](https://github.com/raydium-io/raydium-clmm)                             |
| Orca Whirlpool   | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc`  | [programs/whirlpool/src/lib.rs](https://github.com/orca-so/whirlpools)                            |
| Meteora DLMM     | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`  | [ts-client/src/dlmm/constants](https://github.com/MeteoraAg/dlmm-sdk)                             |
| Meteora DAMM v2  | `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG`  | [programs/cp-amm/src/lib.rs](https://github.com/MeteoraAg/damm-v2)                                |
| PancakeSwap CLMM | `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq` | [packages/solana-clmm-sdk/src/clmm/programId.ts](https://github.com/pancakeswap/pancake-frontend) |

### Related Configs (from source repos)

| Program         | Config Account                                 | Purpose          |
| --------------- | ---------------------------------------------- | ---------------- |
| Orca Whirlpool  | `2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ` | WhirlpoolsConfig |
| Meteora DAMM v2 | `HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC` | Pool Authority   |
| Solend          | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo`  | Lending Program  |

## MEV Strategies

### Backrunning (Primary Strategy)

1. Victim swaps A → B on Pool X
2. Pool X price shifts (more A, less B)
3. We buy B cheap on Pool X, sell on Pool Y
4. Bundle: `[victim_tx, arb_tx]`

### Sandwich (Optional, Disabled by Default)

1. Victim swaps A → B with slippage tolerance
2. Front-run: We buy B first (price up)
3. Victim executes at worse price
4. Back-run: We sell B at profit
5. Bundle: `[front_tx, victim_tx, back_tx]`

## Flash Loan Integration

We use Solend flash loans to fund arb legs without capital:

1. Borrow anchor token (SOL/USDC) from Solend
2. Execute swap chain
3. Repay loan + fee
4. Keep profit

### Solend Flash Loan Details

| Component     | Value                                                                 |
| ------------- | --------------------------------------------------------------------- |
| Program ID    | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo`                         |
| Instructions  | `FlashBorrowReserveLiquidity` (19), `FlashRepayReserveLiquidity` (20) |
| Fee Structure | `flash_loan_fee_wad` (0.3% default = 3_000_000_000_000_000)           |
| Fee Receiver  | Configured per reserve                                                |

Flash loan reserves are configured in `solend_flash_loans` with full account addresses.

## Configuration

```json
{
  "shredstream": {
    "endpoint": "http://127.0.0.1:10000",
    "mev": {
      "backrun_enabled": true,
      "min_swap_amount": 10000000,
      "min_profit_lamports": 50000,
      "tip_profit_share_bps": 5000
    }
  },
  "geyser": {
    "enabled": true,
    "endpoint": "http://localhost:10000",
    "skip_votes": true
  }
}
```

## Module Structure

```
src/
├── geyser.rs                # Geyser gRPC client (ground truth)
├── pool_discovery.rs        # Dynamic pool state tracking via Geyser
├── predictive_state.rs      # Unified state manager (THE key component)
├── tip_monitor.rs           # Jito tip floor monitoring
├── multi_region.rs          # Multi-region parallel bundle submission
├── shredstream/
│   ├── unified_pipeline.rs  # Main pipeline (ShredStream + Geyser)
│   ├── parser.rs            # Transaction → ParsedSwap
│   ├── evaluator.rs         # ParsedSwap → MevOpportunity
│   ├── backrun.rs           # Backrun bundle construction
│   └── programs.rs          # DEX program IDs
├── turbine/                 # Direct UDP shred receiver (optional)
│   ├── receiver.rs          # TVU UDP packet handling
│   ├── shred.rs             # Shred header parsing
│   ├── fec.rs               # Reed-Solomon FEC decoding
│   ├── assembler.rs         # Entry reconstruction
│   └── integration.rs       # Hybrid pipeline integration
├── executor.rs              # Transaction building & signing
└── jito.rs                  # Bundle submission
```

## Predictive State Manager

The `PredictiveStateManager` is the key component that bridges ShredStream and Geyser:

```
┌─────────────────────────────────────────────────────────────────┐
│                    PredictiveStateManager                       │
├─────────────────────────────────────────────────────────────────┤
│  ConfirmedState ◄── Geyser AccountUpdates (ground truth)        │
│       │                                                         │
│       ▼                                                         │
│  PendingSwaps ◄── ShredStream entries (early detection)         │
│       │                                                         │
│       ▼                                                         │
│  PredictedState = ConfirmedState + simulate(PendingSwaps)       │
└─────────────────────────────────────────────────────────────────┘
```

### Key Invariants

1. **ConfirmedState** is ONLY updated by Geyser (never ShredStream)
2. **PendingSwaps** are added by ShredStream, removed by Geyser confirmation
3. **PredictedState** is recomputed on every state change
4. **MEV evaluation** ALWAYS uses PredictedState, never ConfirmedState directly

## Running

```bash
# Ensure local validator is running with Geyser plugin
# AND ShredStream proxy is running
# (See docs/rpc_node_setup.md for setup)

# Run the bot
./target/release/tre-arb-bot -c config.json
```

Logs are written to `/tmp/tre-arb.log`.

## Pipeline Statistics

The unified pipeline tracks comprehensive statistics:

```
SHRED[entries=X txs=X swaps=X] GEYSER[txs=X accts=X] PENDING[add=X conf=X exp=X cur=X] MEV[opps=X submitted=X accepted=X] profit=X.XX SOL
```

| Stat                | Description                          |
| ------------------- | ------------------------------------ |
| `shred_entries`     | Entries received from ShredStream    |
| `shred_swaps`       | Swaps detected via ShredStream       |
| `geyser_txs`        | Transactions confirmed via Geyser    |
| `pending_added`     | Swaps added to pending state         |
| `pending_confirmed` | Pending swaps confirmed by Geyser    |
| `pending_expired`   | Pending swaps expired (TTL exceeded) |
| `current_pending`   | Current pending swap count           |
| `opps`              | MEV opportunities found              |
| `submitted`         | Bundles submitted to Jito            |
| `accepted`          | Bundles accepted by Jito             |
| `profit`            | Total profit in SOL                  |

## Risks & Mitigations

| Risk                   | Mitigation                                                          |
| ---------------------- | ------------------------------------------------------------------- |
| Victim tx fails        | Bundle fails atomically                                             |
| Stale pool state       | Real-time updates via Geyser AccountUpdates + predictive simulation |
| Competing searchers    | Higher tips via tip floor monitor, faster processing                |
| Flash loan unavailable | Skip opportunity                                                    |
| Network latency        | Local validator + ShredStream eliminates external API latency       |
| Pending swap expires   | TTL-based cleanup (5 seconds default)                               |
| State prediction error | Geyser ground truth corrects prediction on confirmation             |
