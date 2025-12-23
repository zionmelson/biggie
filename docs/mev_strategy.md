# MEV Strategy Architecture

## Overview

This bot extracts MEV from Solana using a **Geyser-first architecture**. By connecting directly to your local validator's Geyser plugin, we intercept transactions as they're processed, enabling ultra-low-latency backrunning.

## Architecture

```
┌─────────────────────┐
│   Local Validator   │
│   (Geyser Plugin)   │
└─────────────────────┘
         │
         │ gRPC: TransactionUpdates
         ▼
┌─────────────────────┐
│   Geyser Client     │  Subscribe to transaction stream
└─────────────────────┘
         │
         │ TransactionUpdate { slot, signature, transaction }
         ▼
┌─────────────────────┐
│  Transaction Parser │  Identify DEX swaps
└─────────────────────┘
         │
         │ ParsedSwap { pool, mints, amounts, direction }
         ▼
┌─────────────────────┐
│   MEV Evaluator     │  Filter profitable opportunities
└─────────────────────┘
         │
         │ MevOpportunity { victim_tx, arb_route, profit }
         ▼
┌─────────────────────┐
│   Bundle Builder    │  [victim, arb] backrun bundle
└─────────────────────┘
         │
         ▼
┌─────────────────────┐
│   Jito Submitter    │  Submit bundle with tip
└─────────────────────┘
```

## Timing Advantage

With Geyser, we receive transactions as they're being processed by the validator:

```
Transaction lifecycle:
├── t=0ms     TX received by validator
├── t=1-5ms   Geyser streams TX to bot ◄── Our window
├── t=5-10ms  We parse + evaluate + build bundle
├── t=10-20ms Bundle submitted to Jito
├── t=400ms   Block finalized
└── t=500ms   Confirmation propagates
```

## Supported DEXes

| DEX | Program ID | Source |
|-----|-----------|--------|
| Raydium AMM | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` | [program/src/lib.rs](https://github.com/raydium-io/raydium-amm) |
| Raydium CLMM | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` | [programs/amm/src/lib.rs](https://github.com/raydium-io/raydium-clmm) |
| Orca Whirlpool | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc` | [programs/whirlpool/src/lib.rs](https://github.com/orca-so/whirlpools) |
| Meteora DLMM | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` | [ts-client/src/dlmm/constants](https://github.com/MeteoraAg/dlmm-sdk) |
| Meteora DAMM v2 | `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG` | [programs/cp-amm/src/lib.rs](https://github.com/MeteoraAg/damm-v2) |
| PancakeSwap CLMM | `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq` | [packages/solana-clmm-sdk/src/clmm/programId.ts](https://github.com/pancakeswap/pancake-frontend) |

### Related Configs (from source repos)

| Program | Config Account | Purpose |
|---------|---------------|---------|
| Orca Whirlpool | `2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ` | WhirlpoolsConfig |
| Meteora DAMM v2 | `HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC` | Pool Authority |
| Solend | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo` | Lending Program |

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

| Component | Value |
|-----------|-------|
| Program ID | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo` |
| Instructions | `FlashBorrowReserveLiquidity` (19), `FlashRepayReserveLiquidity` (20) |
| Fee Structure | `flash_loan_fee_wad` (0.3% default = 3_000_000_000_000_000) |
| Fee Receiver | Configured per reserve |

Flash loan reserves are configured in `solend_flash_loans` with full account addresses.

## Configuration

```json
{
  "geyser": {
    "enabled": true,
    "endpoint": "http://localhost:10000",
    "skip_votes": true
  },
  "shredstream": {
    "mev": {
      "backrun_enabled": true,
      "sandwich_enabled": false,
      "min_swap_amount": 10000000,
      "min_profit_lamports": 50000,
      "tip_profit_share_bps": 5000
    }
  }
}
```

## Module Structure

```
src/
├── geyser.rs                # Geyser gRPC client (primary data source)
├── pool_discovery.rs        # Dynamic pool state tracking
├── shredstream/
│   ├── geyser_pipeline.rs   # Main Geyser processing loop
│   ├── parser.rs            # Transaction → ParsedSwap
│   ├── evaluator.rs         # ParsedSwap → MevOpportunity
│   ├── backrun.rs           # Backrun bundle construction
│   ├── programs.rs          # DEX program IDs
│   └── types.rs             # Shared types
├── executor.rs              # Transaction building & signing
└── jito.rs                  # Bundle submission
```

## Running

```bash
# Ensure local validator is running with Geyser plugin
# (See docs/rpc_node_setup.md for validator setup)

# Run the bot
cd /home/ubuntu/tre
./target/release/tre-arb-bot -c config.json
```

Logs are written to `/tmp/tre-arb.log`.

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Victim tx fails | Bundle fails atomically |
| Stale pool state | Real-time updates via Geyser AccountUpdates |
| Competing searchers | Higher tips, faster processing |
| Flash loan unavailable | Skip opportunity |
| Network latency | Local validator eliminates external API latency |
