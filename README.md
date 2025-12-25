# tre MEV Bot

`tre` is a high-performance Solana MEV bot using a **unified ShredStream + Geyser architecture** for ultra-low-latency transaction interception. It combines early transaction detection via ShredStream with ground-truth confirmation from Geyser, using a predictive state manager to bridge the gap for optimal MEV extraction.

## Feature Highlights

- **Unified ShredStream + Geyser architecture:** ShredStream provides early TX detection (~0-5ms), Geyser provides ground truth confirmation—both work together via `PredictiveStateManager`.
- **Predictive state management:** Simulates pending swaps before confirmation for accurate MEV evaluation with state prediction.
- **Direct Turbine UDP deshredding:** Optional bypass of ShredStream proxy for minimum latency (~0.5-2ms vs ~5-10ms).
- **Multi-protocol support:** Parses swaps from Raydium AMM/CLMM, Orca Whirlpool, Meteora DLMM/DAMM, and PancakeSwap CLMM.
- **Flash loan integration:** Uses Solend flash loans for capital-efficient arbitrage execution.
- **Jito bundle submission:** Atomic bundle submission via Jito block engine with dynamic tip floor monitoring.
- **Dynamic pool discovery:** Automatically discovers and tracks pool states from on-chain Geyser account updates.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                       UNIFIED MEV PIPELINE                              │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  SHREDSTREAM TASK                    GEYSER TASK                        │
│  (Early TX Detection)                (Ground Truth)                     │
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

### Core Components

- **`UnifiedMevPipeline`** ([src/shredstream/unified_pipeline.rs](src/shredstream/unified_pipeline.rs)): Main pipeline orchestrating ShredStream + Geyser integration.
- **`PredictiveStateManager`** ([src/predictive_state.rs](src/predictive_state.rs)): Unified state manager bridging early detection and ground truth—the single source of truth for pool state.
- **`GeyserClient`** ([src/geyser.rs](src/geyser.rs)): gRPC client for transaction, account, and slot streaming from local validator.
- **`TurbineReceiver`** ([src/turbine/](src/turbine/)): Direct UDP shred receiver for minimum latency (bypasses ShredStream proxy).
- **`VaultTracker`** ([src/pool_discovery.rs](src/pool_discovery.rs)): Dynamic pool discovery and state tracking via Geyser account updates.
- **`TipFloorMonitor`** ([src/tip_monitor.rs](src/tip_monitor.rs)): Real-time Jito tip floor monitoring for competitive tipping.
- **`JitoClient`** ([src/jito.rs](src/jito.rs)): Bundle submission to Jito block engine.
- **`BackrunBundleBuilder`** ([src/shredstream/backrun.rs](src/shredstream/backrun.rs)): Constructs atomic backrun bundles with flash loans.

## Prerequisites

- Rust `1.77` or newer (Rustup recommended).
- `protoc` in `PATH` for compiling Geyser/Jito protobufs.
- **Local Solana validator** with Geyser plugin (Yellowstone gRPC recommended).
- **ShredStream proxy** running locally (connects to Jito for early TX detection).
- Jito auth keypair for bundle submission.

## Building & Running

```bash
# Build
cargo build --release

# Run with config
./target/release/tre-arb-bot -c config.json
```

Logs are written to `/tmp/tre-arb.log`. Press `Ctrl+C` to stop.

## Configuration (`config.json`)

### ShredStream Settings (Early TX Detection)

```json
{
  "shredstream": {
    "endpoint": "http://127.0.0.1:10000",
    "mev": {
      "backrun_enabled": true,
      "min_swap_amount": 10000000,
      "min_profit_lamports": 50000,
      "min_victim_slippage_bps": 30,
      "compute_units": 400000,
      "compute_unit_price": 1000,
      "tip_profit_share_bps": 5000
    }
  }
}
```

### Geyser Settings (Ground Truth Confirmation)

```json
{
  "geyser": {
    "enabled": true,
    "endpoint": "http://localhost:10000",
    "skip_votes": true,
    "connect_timeout_ms": 10000,
    "request_timeout_ms": 30000
  }
}
```

### Local RPC (For Pool State Queries)

```json
{
  "local_rpc": {
    "enabled": true,
    "url": "http://localhost:8899",
    "ws_url": "ws://localhost:8900"
  }
}
```

### Jito Configuration

```json
{
  "jito": {
    "endpoints": ["https://ny.mainnet.block-engine.jito.wtf"]
  },
  "wallets": {
    "auth": { "path": "/path/to/jito-auth.json" },
    "payer": { "path": "/path/to/payer.json" }
  }
}
```

### MEV Strategy Settings

```json
{
  "shredstream": {
    "endpoint": "http://127.0.0.1:10000",
    "mev": {
      "backrun_enabled": true,
      "min_swap_amount": 10000000,
      "min_profit_lamports": 50000,
      "min_victim_slippage_bps": 30,
      "compute_units": 400000,
      "compute_unit_price": 1000,
      "tip_profit_share_bps": 5000
    }
  }
}
```

### Flash Loan Reserves (Solend)

```json
{
  "solend_flash_loans": {
    "wsol": {
      "program_id": "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo",
      "lending_market": "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF",
      "reserve": "8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36",
      "reserve_liquidity_mint": "So11111111111111111111111111111111111111112",
      "borrow": {
        "reserve_liquidity_supply": "ADJqxHKRYkjYMrBC5i1Y5Xqz5SEZjwZw3aPqJChVekWE"
      },
      "repay": {
        "reserve_liquidity_supply": "ADJqxHKRYkjYMrBC5i1Y5Xqz5SEZjwZw3aPqJChVekWE",
        "reserve_liquidity_fee_receiver": "Ae3SqmChHqV6uazP3q9aExbv5G6s6zuCuphg9D1bWjDF",
        "host_fee_receiver": "5wo1tFpi4HaVKnemqaXeQnBEpezrJXcXvuztYycLVtLT"
      },
      "max_borrow": 100000000000,
      "min_borrow": 100000000
    }
  }
}
```

## How It Works

1. **Dual Data Sources:** Connects to both ShredStream (for early TX detection) and Geyser (for ground truth confirmation).

2. **Early Transaction Detection (ShredStream):**

   - Receives shred entries before block confirmation
   - Parses transactions for DEX swap instructions
   - Adds detected swaps to `PredictiveStateManager` as pending

3. **Ground Truth Confirmation (Geyser):**

   - Subscribes to `TransactionUpdates` for TX confirmation
   - Subscribes to `AccountUpdates` for vault balance changes
   - Confirms/removes pending swaps and updates confirmed state

4. **Predictive State Management:**

   - Maintains `ConfirmedState` (from Geyser) + `PendingSwaps` (from ShredStream)
   - Computes `PredictedState = ConfirmedState + simulate(PendingSwaps)`
   - MEV evaluation ALWAYS uses predicted state for accuracy

5. **MEV Evaluation:** Filters opportunities based on:

   - Minimum swap amount threshold
   - Available predicted pool state
   - Expected profit after fees and tips

6. **Bundle Construction:** For profitable opportunities:

   - Builds flash loan borrow instruction (Solend)
   - Constructs backrun swap instruction
   - Adds flash loan repay instruction
   - Includes dynamic Jito tip (from tip floor monitor)

7. **Bundle Submission:** Submits atomic bundle to Jito block engine with competitive tip.

## Project Structure

```
src/
├── main.rs              # Entry point (unified pipeline mode)
├── lib.rs               # Library exports
├── config.rs            # Configuration parsing
├── geyser.rs            # Geyser gRPC client (ground truth)
├── pool_discovery.rs    # Dynamic pool discovery via Geyser
├── predictive_state.rs  # Unified state manager (ShredStream + Geyser)
├── tip_monitor.rs       # Jito tip floor monitoring
├── multi_region.rs      # Multi-region parallel bundle submission
├── amm_math.rs          # AMM swap math (constant product, slippage)
├── clmm_math.rs         # CLMM tick/sqrt_price math
├── dlmm_math.rs         # DLMM bin math (Meteora)
├── executor.rs          # Transaction building
├── jito.rs              # Jito bundle submission
├── constants.rs         # Program IDs and constants
├── shredstream/
│   ├── mod.rs           # Module exports
│   ├── unified_pipeline.rs  # Main pipeline (ShredStream + Geyser)
│   ├── parser.rs        # Transaction → ParsedSwap
│   ├── evaluator.rs     # MEV opportunity evaluation
│   ├── backrun.rs       # Bundle construction
│   └── programs.rs      # DEX program IDs
├── turbine/
│   ├── mod.rs           # Direct UDP shred receiver
│   ├── receiver.rs      # TVU UDP packet handling
│   ├── shred.rs         # Shred header parsing
│   ├── fec.rs           # Reed-Solomon FEC decoding
│   ├── assembler.rs     # Entry reconstruction
│   └── integration.rs   # Hybrid pipeline integration
└── bin/
    ├── build_array_map.rs   # Build static lookup maps
    ├── test_jito_pipeline.rs # Test Jito integration
    └── test_pda.rs          # Test PDA derivation

geyser/                  # Geyser protobuf definitions
jito/                    # Jito protobuf definitions
solend/                  # Solend CPI library
docs/
├── mev_strategy.md      # MEV strategy documentation
├── error_codes.md       # Protocol error reference
└── rpc_node_setup.md    # Validator setup guide
```

## Timing Advantage

```
Transaction lifecycle (unified pipeline):
├── t=0ms      TX enters validator mempool
├── t=0-2ms    ShredStream: Entry received → swap detected → added to PendingSwaps
├── t=1-3ms    PredictedState recalculated (includes victim's impact)
├── t=2-5ms    MEV Evaluator uses PredictedState → finds opportunity
├── t=3-8ms    Bundle built and submitted to Jito
├── t=5-15ms   Geyser: Victim TX confirmed → PendingSwap removed
├── t=400ms    Block finalized
└── t=500ms    Confirmation propagates

Direct Turbine UDP (optional, even lower latency):
├── t=0ms      Shred broadcast from validator TVU
├── t=0.5-2ms  Direct UDP receive → FEC decode → Entry assembled
├── t=1-3ms    Swap detected → MEV evaluation
```

## Further Reading

See [docs/mev_strategy.md](docs/mev_strategy.md) for MEV strategy details, and [docs/rpc_node_setup.md](docs/rpc_node_setup.md) for local validator setup.
