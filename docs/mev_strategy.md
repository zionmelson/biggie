# MEV Strategy Architecture

## Overview

This bot extracts MEV from Solana by intercepting pending transactions via Jito Shredstream. We see swaps 200-400ms before confirmation, enabling backrunning and sandwich attacks.

## Architecture

```
Jito Block Engine
         │
         │ Shreds (UDP)
         ▼
┌─────────────────────┐
│  Shredstream Proxy  │  localhost:20000 UDP, :10000 gRPC
└─────────────────────┘
         │
         │ gRPC: Entry { slot, transactions[] }
         ▼
┌─────────────────────┐
│  Transaction Parser │  Identify DEX swaps
└─────────────────────┘
         │
         │ ParsedSwap { pool, mints, amounts }
         ▼
┌─────────────────────┐
│   MEV Evaluator     │  Filter profitable opportunities
└─────────────────────┘
         │
         │ MevOpportunity { victim_tx, arb_route }
         ▼
┌─────────────────────┐
│   Bundle Builder    │  [victim, arb] or [front, victim, back]
└─────────────────────┘
         │
         ▼
┌─────────────────────┐
│   Jito Submitter    │  Submit bundle with tip
└─────────────────────┘
```

## Timing Advantage

```
Slot N timeline:
├── t=0ms     Leader starts building
├── t=50ms    First shreds broadcast
├── t=100ms   We receive shreds ◄── Window opens
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

### Backrunning

1. Victim swaps A → B on Pool X
2. Pool X price shifts (more A, less B)
3. We buy B cheap on Pool X, sell on Pool Y
4. Bundle: `[victim_tx, arb_tx]`

### Sandwich

1. Victim swaps A → B with slippage tolerance
2. Front-run: We buy B first (price up)
3. Victim executes at worse price
4. Back-run: We sell B at profit
5. Bundle: `[front_tx, victim_tx, back_tx]`

## Flash Loan Integration

We use flash loans to fund arb legs without capital:

1. Borrow anchor token (USDC/SOL) from Solend
2. Execute swap chain
3. Repay loan + fee
4. Keep profit

### Solend Flash Loan Details (verified from source)

| Component | Value |
|-----------|-------|
| Program ID | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo` |
| Instructions | `FlashBorrowReserveLiquidity` (19), `FlashRepayReserveLiquidity` (20) |
| Fee Structure | `flash_loan_fee_wad` (0.3% default = 3_000_000_000_000_000) |
| Fee Receiver | Configured per reserve |

Anchor tokens configured in `solend_flash_loans` with reserve addresses.

## Configuration

```json
{
  "shredstream": {
    "enabled": true,
    "endpoint": "http://127.0.0.1:10000"
  }
}
```

## Module Structure

```
src/
├── shredstream.rs           # gRPC client
├── shredstream/
│   ├── programs.rs          # DEX program IDs
│   ├── parser.rs            # Transaction → ParsedSwap
│   ├── evaluator.rs         # ParsedSwap → MevOpportunity
│   ├── backrun.rs           # Backrun bundle construction
│   └── pipeline.rs          # Main processing loop
├── executor.rs              # Transaction building & signing
└── jito.rs                  # Bundle submission
```

## Running

```bash
# Terminal 1: Shredstream proxy
cd /home/ubuntu/shredstream-proxy
./target/release/jito-shredstream-proxy shredstream \
    --block-engine-url https://mainnet.block-engine.jito.wtf \
    --auth-keypair /home/ubuntu/cain/wallets/jito-auth.json \
    --desired-regions ny \
    --dest-ip-ports 127.0.0.1:8001 \
    --grpc-service-port 10000

# Terminal 2: Bot
cd /home/ubuntu/tre
./target/release/tre-arb-bot --config ./config.json
```

## Risks

| Risk | Mitigation |
|------|-----------|
| Victim tx fails | Bundle fails atomically |
| Stale pool state | Refresh via RPC for high-value |
| Competing searchers | Higher tips |
| Flash loan unavailable | Skip opportunity |
