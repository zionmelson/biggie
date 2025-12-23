# tre Arbitrage Monitor

`tre` is a real-time Solana arbitrage opportunity monitor that ingests Bitquery CoreCast swap streams, tracks cross-protocol price discrepancies, and surfaces profitable flash-loan opportunities via an interactive TUI dashboard.

## Feature Highlights

- **Real-time arbitrage detection:** Streams DEX trades from CoreCast and identifies cross-protocol price discrepancies in real-time.
- **Interactive TUI:** Ratatui-powered terminal interface displays live arbitrage opportunities with profit percentages and timing.
- **Multi-protocol support:** Monitors prices across Raydium, Orca, Meteora, and other Solana DEXes via unified CoreCast stream.
- **Anchor token portfolio:** Configurable set of flash-borrowable tokens (USDC, SOL, USDT, etc.) derived from Solend reserve configurations.
- **Price tracking:** Maintains rolling price history per token pair per protocol for accurate spread detection.

## Architecture

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│   CoreCast      │────▶│  ArbitrageMonitor│────▶│   TUI Display   │
│  (gRPC Stream)  │     │  (Price Tracker) │     │   (Ratatui)     │
└─────────────────┘     └──────────────────┘     └─────────────────┘
        │                        │
        ▼                        ▼
  DexTradeEvent          ArbitrageOpportunity
  - buy/sell mints       - token pair
  - amounts/decimals     - buy/sell protocols
  - protocol name        - profit percentage
```

### Core Components

- **`CorecastClient`** (`src/corecast.rs`): gRPC client connecting to Bitquery CoreCast, subscribing to `dex_trades` streams filtered by anchor token mints.
- **`ArbitrageMonitor`** (`src/arb_monitor.rs`): Consumes trade events, normalizes prices, tracks per-protocol price history, and detects arbitrage when the same token pair trades at different prices across protocols.
- **`main.rs`**: Ratatui TUI application displaying the 20 most recent opportunities with real-time updates.

## Prerequisites

- Rust `1.77` or newer (Rustup recommended).
- `protoc` in `PATH` for compiling Bitquery protobufs (required by `build.rs`).
- CoreCast API credentials (Bitquery authorization token).
- TLS CA certificate for CoreCast endpoint.

## Building & Running

```bash
# Build
cargo build --release

# Run with custom config
./target/release/tre-arb-bot --config ./config.json

# Or during development
cargo run -- --config ./config.json
```

Press `q` to quit the TUI.

## Configuration (`config.json`)

### CoreCast Settings

```json
{
  "corecast": {
    "address": "corecast.bitquery.io",
    "authorization": "<your-bitquery-token>",
    "ca_pem": "certs/corecast_ca.pem",
    "cache": {
      "warm_capacity": 32,
      "channel_capacity": 512
    }
  }
}
```

### Anchor Tokens

Anchor tokens are automatically derived from `solend_flash_loans`. Each reserve's `reserve_liquidity_mint` becomes a monitored token:

```json
{
  "solend_flash_loans": {
    "USDC": {
      "reserve_liquidity_mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
    },
    "SOL": {
      "reserve_liquidity_mint": "So11111111111111111111111111111111111111112"
    }
  }
}
```

### Jito Configuration (for future execution)

```json
{
  "jito": {
    "endpoints": ["https://ny.mainnet.block-engine.jito.wtf"],
    "ca_pem": "certs/jito_block_engine_ca.pem",
    "tip_floor_url": "https://bundles.jito.wtf/api/v1/bundles/tip_floor"
  },
  "wallets": {
    "auth": { "path": "/path/to/jito-auth.json" },
    "payer": { "path": "/path/to/jito-payer.json" }
  }
}
```

### Planner Settings

```json
{
  "planner": {
    "min_profit_lamports": 1,
    "max_hops": 3,
    "max_queue_depth": 4096
  }
}
```

## How It Works

1. **Stream Subscription**: Connects to CoreCast and subscribes to `dex_trades` filtered by anchor token mints.

2. **Trade Processing**: For each `DexTradeEvent`, extracts:

   - Buy/sell token mints and amounts
   - Protocol name (Raydium, Orca, etc.)
   - Calculates normalized price (quote per base)

3. **Price Tracking**: Maintains a rolling window of 100 price points per (protocol, token_pair) combination.

4. **Arbitrage Detection**: When the same token pair has recent trades on different protocols:

   - Compares prices across protocols
   - Calculates spread percentage
   - Flags opportunities above threshold

5. **Display**: Updates the TUI with opportunities showing:
   - Token pair
   - Buy protocol (lower price)
   - Sell protocol (higher price)
   - Profit percentage
   - Time since detection

## Project Structure

```
src/
├── main.rs           # TUI application entry point
├── lib.rs            # Library exports
├── arb_monitor.rs    # Arbitrage detection logic
├── config.rs         # Configuration parsing
├── corecast.rs       # CoreCast gRPC client
├── solana_corecast.rs    # Generated protobuf types
└── solana_messages.rs    # Generated protobuf types

bitquery/
└── solana/           # CoreCast protobuf definitions
    └── corecast/     # Streaming message types

docs/
└── arbitrage_strategy.md  # Strategy design document
```

## Future Roadmap

- [ ] Flash-loan execution via Solend
- [ ] Jito bundle submission for atomic arbitrage
- [ ] Multi-hop path finding (A → B → C → A cycles)
- [ ] Prometheus metrics export
- [ ] Historical opportunity logging

## Further Reading

See `docs/arbitrage_strategy.md` for the complete arbitrage strategy design, including flash-loan flows, anchor token portfolios, and risk safeguards.
