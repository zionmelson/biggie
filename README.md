# tre MEV Bot

`tre` is a high-performance Solana MEV bot using a **Geyser-first architecture** for ultra-low-latency transaction interception. It connects directly to your local validator's Geyser plugin, enabling real-time backrunning and MEV extraction with minimal latency.

## Feature Highlights

- **Geyser-first architecture:** Direct connection to local validator's Geyser plugin for ultra-low-latency transaction streaming.
- **Real-time MEV detection:** Intercepts pending transactions and identifies profitable backrun opportunities.
- **Multi-protocol support:** Parses swaps from Raydium AMM/CLMM, Orca Whirlpool, Meteora DLMM/DAMM, and PancakeSwap CLMM.
- **Flash loan integration:** Uses Solend flash loans for capital-efficient arbitrage execution.
- **Jito bundle submission:** Atomic bundle submission via Jito block engine for reliable MEV extraction.
- **Dynamic pool discovery:** Automatically discovers and tracks pool states from on-chain data.

## Architecture

```
┌─────────────────────┐
│  Local Validator    │
│  (Geyser Plugin)    │
└─────────────────────┘
         │
         │ gRPC: TransactionUpdates, AccountUpdates
         ▼
┌─────────────────────┐
│   Geyser Pipeline   │  Parse → Evaluate → Build → Submit
└─────────────────────┘
         │
         ├──▶ Parser: Identify DEX swaps
         ├──▶ Evaluator: Filter profitable opportunities
         ├──▶ Bundle Builder: Construct backrun bundles
         └──▶ Jito Submitter: Submit with tip
                    │
                    ▼
           ┌─────────────────┐
           │ Jito Block      │
           │ Engine          │
           └─────────────────┘
```

### Core Components

- **`GeyserClient`** ([src/geyser.rs](src/geyser.rs)): gRPC client connecting to local validator's Geyser plugin for transaction and account streaming.
- **`GeyserPipeline`** ([src/shredstream/geyser_pipeline.rs](src/shredstream/geyser_pipeline.rs)): Main processing loop that parses transactions, evaluates MEV opportunities, and submits bundles.
- **`PoolDiscoveryService`** ([src/pool_discovery.rs](src/pool_discovery.rs)): Dynamic pool discovery and state tracking via Geyser account updates.
- **`JitoClient`** ([src/jito.rs](src/jito.rs)): Bundle submission to Jito block engine.
- **`BackrunBundleBuilder`** ([src/shredstream/backrun.rs](src/shredstream/backrun.rs)): Constructs atomic backrun bundles with flash loans.

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

### Geyser Settings (Primary Data Source)

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
    "enabled": false,
    "mev": {
      "backrun_enabled": true,
      "sandwich_enabled": false,
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

1. **Geyser Connection:** Connects to local validator's Geyser plugin and subscribes to `TransactionUpdates`.

2. **Transaction Parsing:** For each transaction:

   - Identifies DEX swap instructions (Raydium, Orca, Meteora, PancakeSwap)
   - Extracts pool address, input/output mints and amounts
   - Determines swap direction

3. **MEV Evaluation:** Filters opportunities based on:

   - Minimum swap amount threshold
   - Available pool state data
   - Expected profit after fees and tips

4. **Bundle Construction:** For profitable opportunities:

   - Builds flash loan borrow instruction (Solend)
   - Constructs backrun swap instruction
   - Adds flash loan repay instruction
   - Includes Jito tip

5. **Bundle Submission:** Submits atomic bundle to Jito block engine.

## Project Structure

```
src/
├── main.rs              # Entry point (Geyser mode)
├── lib.rs               # Library exports
├── config.rs            # Configuration parsing
├── geyser.rs            # Geyser gRPC client
├── pool_discovery.rs    # Dynamic pool discovery (replaces static JSON maps)
├── predictive_state.rs  # Unified state manager (ShredStream + Geyser)
├── amm_math.rs          # AMM swap math (constant product, slippage)
├── clmm_math.rs         # CLMM tick/sqrt_price math
├── dlmm_math.rs         # DLMM bin math (Meteora)
├── executor.rs          # Transaction building
├── jito.rs              # Jito bundle submission
├── tip_monitor.rs       # Jito tip floor monitoring
├── constants.rs         # Program IDs and constants
└── shredstream/
    ├── mod.rs           # Module exports
    ├── unified_pipeline.rs  # Main pipeline (ShredStream + Geyser)
    ├── parser.rs        # Transaction → ParsedSwap
    ├── evaluator.rs     # MEV opportunity evaluation
    ├── backrun.rs       # Bundle construction
    ├── programs.rs      # DEX program IDs
    └── types.rs         # Shared types

geyser/                  # Geyser protobuf definitions
jito/                    # Jito protobuf definitions
solend/                  # Solend CPI library
docs/
├── mev_strategy.md      # MEV strategy documentation
├── error_codes.md       # Protocol error reference
└── rpc_node_setup.md    # Validator setup guide
```

## Further Reading

See [docs/mev_strategy.md](docs/mev_strategy.md) for MEV strategy details, and [docs/rpc_node_setup.md](docs/rpc_node_setup.md) for local validator setup.
