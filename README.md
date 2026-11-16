# AI GENERATED CODE DO NOT WASTE TIME READING

## tre Arb Bot

`tre` is a high-performance Solana cross-pool arbitrage bot using a **unified Turbine + Geyser architecture**. It detects state changes from confirmed transactions via low-latency shred data, then executes cross-pool arbitrage when price discrepancies exist between pools.

## Feature Highlights

- **Unified Turbine + Geyser architecture:** Turbine UDP provides fast state updates (~0.5-2ms), Geyser provides authoritative vault balance confirmation—both work together via `PoolStateManager`.
- **Pool state management:** Tracks pool states from Geyser account updates for accurate arbitrage evaluation.
- **Direct Turbine UDP deshredding:** Minimum latency shred reception (~0.5-2ms vs ~5-10ms from proxy).
- **Multi-protocol support:** Parses swaps from Raydium AMM/CLMM, Orca Whirlpool, Meteora DLMM/DAMM, and PancakeSwap CLMM.
- **Flash loan integration:** Uses Solend flash loans for capital-efficient arbitrage execution.
- **Jito bundle submission:** Atomic bundle submission via Jito block engine with dynamic tip floor monitoring.
- **Dynamic pool discovery:** Automatically discovers and tracks pool states from on-chain Geyser account updates.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                       UNIFIED ARB PIPELINE                              │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  TURBINE/SHREDSTREAM TASK             GEYSER TASK                       │
│  (Fast State Updates)                 (Authoritative State)             │
│  • Receive shreds                     • Subscribe transactions          │
│  • Parse transactions                 • Subscribe accounts (vaults)     │
│  • Detect state changes               • Subscribe slots                 │
│         │                                   │                           │
│         ▼                                   ▼                           │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                POOL STATE MANAGER (shared)                       │   │
│  │  • Updates pool state ◄── Turbine shreds (fast)                 │   │
│  │  • Confirms state ◄── Geyser account updates (authoritative)    │   │
│  │  • Tracks vault balances for all monitored pools                │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                              │                                          │
│                              ▼                                          │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │              ARB EVALUATOR (uses pool state)                     │   │
│  │  • Evaluates cross-pool arbitrage opportunities                 │   │
│  │  • Finds routes where price discrepancies exist                 │   │
│  │  • Calculates profit after fees and slippage                    │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                              │                                          │
│                              ▼                                          │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │              ARB BUNDLE BUILDER & JITO SUBMISSION                │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### Core Components

- **`UnifiedArbPipeline`** ([src/shredstream/unified_pipeline.rs](src/shredstream/unified_pipeline.rs)): Main pipeline orchestrating Turbine + Geyser integration.
- **`PoolStateManager`** ([src/pool_state.rs](src/pool_state.rs)): Unified state manager tracking pool states—the single source of truth for pool reserves.
- **`GeyserClient`** ([src/geyser.rs](src/geyser.rs)): gRPC client for account, block, and slot streaming from local validator.
- **`TurbineReceiver`** ([src/turbine/](src/turbine/)): Direct UDP shred receiver for minimum latency.
- **`VaultTracker`** ([src/pool_discovery.rs](src/pool_discovery.rs)): Dynamic pool discovery and state tracking via Geyser account updates.
- **`TipFloorMonitor`** ([src/tip_monitor.rs](src/tip_monitor.rs)): Real-time Jito tip floor monitoring for competitive tipping.
- **`JitoClient`** ([src/jito.rs](src/jito.rs)): Bundle submission to Jito block engine.
- **`ArbBundleBuilder`** ([src/shredstream/arb_bundle.rs](src/shredstream/arb_bundle.rs)): Constructs atomic arbitrage bundles with flash loans.

## Prerequisites

- Rust `1.77` or newer (Rustup recommended).
- `protoc` in `PATH` for compiling Geyser/Jito protobufs.
- **Local Solana validator** with Geyser plugin (Yellowstone gRPC recommended).
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

### Jito ShredStream Proxy (Stake-Weighted - DEFAULT)

ShredStream proxy is the **recommended** method for receiving shreds because:
- **Stake-weighted access**: Jito aggregates shreds from high-stake validators
- **Layer 1 Turbine position**: Get effective first-layer access without owning stake
- **Better in remote locations**: Outperforms direct Turbine for unstaked/low-stake setups

```json
{
  "shredstream": {
    "turbine": {
      "enabled": false
    },
    "endpoint": "http://127.0.0.1:10000",
    "arb": {
      "arb_enabled": true,
      "min_profit_lamports": 50000,
      "compute_units": 400000,
      "compute_unit_price": 1000,
      "tip_profit_share_bps": 6500
    }
  }
}
```

**ShredStream Proxy Setup:**

Run the [Jito ShredStream proxy](https://github.com/jito-labs/shredstream-proxy) locally:
```bash
# Get approved at https://web.miniextensions.com/WV3gZjFwqNqITsMufIEp

# Run the proxy (gRPC on port 10000)
RUST_LOG=info cargo run --release --bin jito-shredstream-proxy -- shredstream \
    --block-engine-url https://mainnet.block-engine.jito.wtf \
    --auth-keypair /path/to/jito-auth.json \
    --desired-regions ny,amsterdam \
    --dest-ip-ports 127.0.0.1:8001 \
    --grpc-service-port 10000
```

### Direct Turbine UDP (Only for High-Stake Co-location)

Direct Turbine UDP is only faster if you're co-located with a **high-stake validator** you control.
If your validator is unstaked or low-stake, ShredStream will actually be faster.

```json
{
  "shredstream": {
    "turbine": {
      "enabled": true,
      "bind_addr": "0.0.0.0:8011",
      "receiver_threads": 2,
      "recv_buffer_size": 134217728,
      "shred_channel_size": 10000,
      "max_fec_sets": 1000,
      "fec_timeout_ms": 2000
    },
    "arb": {
      "arb_enabled": true,
      "min_profit_lamports": 50000,
      "compute_units": 400000,
      "compute_unit_price": 1000,
      "tip_profit_share_bps": 6500
    }
  }
}
```

**System Tuning Required for Turbine UDP:**
```bash
# Increase UDP receive buffer max (as root)
sudo sysctl -w net.core.rmem_max=134217728
sudo sysctl -w net.core.rmem_default=134217728

# Make persistent in /etc/sysctl.conf
echo "net.core.rmem_max=134217728" | sudo tee -a /etc/sysctl.conf
echo "net.core.rmem_default=134217728" | sudo tee -a /etc/sysctl.conf
```

**Validator Configuration for Turbine Forwarding:**

To receive shreds directly, configure your validator to forward shreds to the bot:
```bash
# Option 1: Use iptables to mirror UDP traffic from validator's TVU
sudo iptables -t mangle -A PREROUTING -p udp --dport 8002 -j TEE --gateway 127.0.0.1 --port 8011

# Option 2: Run bot on same machine and configure validator's TVU broadcast
# Add to validator startup: --tvu-receive-threads 2
```

### Geyser Settings (Authoritative State)

```json
{
  "geyser": {
    "enabled": true,
    "endpoint": "http://localhost:10000",
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

### Arb Strategy Settings

```json
{
  "shredstream": {
    "endpoint": "http://127.0.0.1:10000",
    "arb": {
      "arb_enabled": true,
      "min_swap_amount": 10000000,
      "min_profit_lamports": 50000,
      "min_trigger_slippage_bps": 30,
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

1. **Dual Data Sources:** Connects to both Turbine/ShredStream (for fast state updates) and Geyser (for authoritative state confirmation).

2. **Fast State Updates (Turbine/ShredStream):**

   - Receives shreds containing confirmed transactions
   - Parses transactions for DEX swap instructions
   - Detects swaps that may create cross-pool price discrepancies

3. **Authoritative State (Geyser):**

   - Subscribes to `SlotUpdates` for slot timing signals
   - Subscribes to `BlockUpdates` for blockhash retrieval
   - Subscribes to `AccountUpdates` for vault balance changes
   - Provides ground truth pool state data

4. **Pool State Management:**

   - Maintains current vault balances for all monitored pools
   - Updates state from Geyser account updates
   - Provides accurate reserves for arbitrage calculations

5. **Arb Evaluation:** Filters opportunities based on:

   - Trigger swap that created price discrepancy
   - Cross-pool routes with profitable spreads
   - Expected profit after fees and tips

6. **Bundle Construction:** For profitable opportunities:

   - Builds flash loan borrow instruction (Solend)
   - Constructs arbitrage swap instruction(s)
   - Adds flash loan repay instruction
   - Includes dynamic Jito tip (from tip floor monitor)

7. **Bundle Submission:** Submits atomic bundle to Jito block engine for next-block inclusion.

## Project Structure

```
src/
├── main.rs              # Entry point (unified pipeline mode)
├── lib.rs               # Library exports
├── config.rs            # Configuration parsing
├── geyser.rs            # Geyser gRPC client (authoritative state)
├── pool_discovery.rs    # Dynamic pool discovery via Geyser
├── pool_state.rs        # Unified pool state manager
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
│   ├── unified_pipeline.rs  # Main pipeline (Turbine + Geyser)
│   ├── parser.rs        # Transaction → ParsedSwap
│   ├── evaluator.rs     # Arbitrage opportunity evaluation
│   ├── arb_bundle.rs    # Bundle construction
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
├── arb_strategy.md      # Arb strategy documentation
├── error_codes.md       # Protocol error reference
└── rpc_node_setup.md    # Validator setup guide
```

## Timing

```
Transaction lifecycle (unified pipeline):
├── t=0ms      Trigger TX included in validator block
├── t=0.5-2ms  Turbine: Shred received → TX parsed → state change detected
├── t=1-3ms    Pool state updated, arb evaluator triggered
├── t=2-5ms    Cross-pool opportunity found, bundle built
├── t=3-8ms    Bundle submitted to Jito for next-block inclusion
├── t=5-15ms   Geyser: Account updates confirm vault balances
├── t=400ms    Next block finalized (arb TX included if successful)
```

## Further Reading

See [docs/arb_strategy.md](docs/arb_strategy.md) for arb strategy details, and [docs/rpc_node_setup.md](docs/rpc_node_setup.md) for local validator setup.
