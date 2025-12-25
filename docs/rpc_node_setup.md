# RPC Node + Geyser + ShredStream Setup for MEV Bot

This document outlines hardware requirements and setup for running a Solana RPC node (non-voting) with Geyser plugin and ShredStream proxy for the tre MEV bot.

## Why Local Validator + Geyser + ShredStream?

The tre bot uses a **unified ShredStream + Geyser architecture** that requires:

1. **Local validator with Yellowstone gRPC Geyser plugin** - Provides ground truth state confirmation
2. **ShredStream proxy** - Provides early transaction detection before confirmation

This combination provides:

- **Ultra-low latency:** ShredStream detects TXs ~0-5ms before confirmation
- **Accurate state:** Geyser provides ground truth via AccountUpdates
- **Predictive evaluation:** PredictiveStateManager bridges early detection and confirmation
- **No rate limits:** Process unlimited transactions per second

## Current Machine Specs

| Component | Current                                            | Status       |
| --------- | -------------------------------------------------- | ------------ |
| CPU       | AMD EPYC 9554P, 64 cores / 128 threads, 3.1GHz     | ✅ Excellent |
| RAM       | 1.5 TiB (1536 GB) DDR5                             | ✅ Excellent |
| Storage   | 4x 7TB Samsung NVMe + 2x 447GB Micron NVMe (~28TB) | ✅ Excellent |
| Network   | 100 Gbps                                           | ✅ Excellent |

---

## Hardware Requirements: Priority Ranking

For MEV bot use case (RPC + Geyser, no voting):

| Priority | Component     | Why It Matters                                               | Minimum   | Sweet Spot |
| -------- | ------------- | ------------------------------------------------------------ | --------- | ---------- |
| **#1**   | **RAM**       | Accounts DB is memory-mapped. Too little = swap death spiral | 256 GB    | 384-512 GB |
| **#2**   | **NVMe IOPS** | Ledger writes, accounts I/O. Random 4K matters most          | 500K IOPS | 1M+ IOPS   |
| **#3**   | **NVMe Size** | Ledger + accounts + snapshots                                | 2 TB      | 4 TB       |
| **#4**   | **Network**   | Gossip, block propagation                                    | 1 Gbps    | 10 Gbps    |
| **#5**   | **CPU**       | Replay, signature verification                               | 8 cores   | 16 cores   |

### RAM Breakdown (Why 256GB Minimum)

```
Accounts DB in RAM:     ~180-220 GB (and growing monthly)
OS + buffers:           ~20 GB
Geyser plugin:          ~10-30 GB (depends on filters)
MEV bot application:    ~5 GB
Safety margin:          ~50 GB
────────────────────────────────────────────────────────
MINIMUM TOTAL:          256 GB
COMFORTABLE:            384 GB
FUTURE-PROOF:           512 GB
```

**Note:** With 1.5 TiB RAM, this machine far exceeds requirements and can comfortably run the accounts DB entirely in memory with substantial headroom.

---

## What You Can Skip (Non-Voting vs Voting)

| Component           | Voting Validator     | RPC Node (Non-Voting) |
| ------------------- | -------------------- | --------------------- |
| Dedicated PoH core  | Critical             | Not needed            |
| TPU/leader threads  | Critical             | Not needed            |
| 99.9% uptime        | Required for rewards | Nice to have          |
| Redundant NVMe RAID | Required             | Single disk OK        |
| CPU cores           | 16+ required         | 8-12 acceptable       |
| Hot standby         | Often needed         | Not needed            |

---

## Current Hardware Status

### ✅ Hardware Requirements Met

The current machine **exceeds all requirements** for running a Solana RPC node with Geyser:

| Requirement | Minimum | Current                | Status          |
| ----------- | ------- | ---------------------- | --------------- |
| RAM         | 256 GB  | 1.5 TiB (1536 GB)      | ✅ 6x minimum   |
| CPU Cores   | 8 cores | 64 cores (128 threads) | ✅ 8x minimum   |
| NVMe Size   | 2 TB    | ~28 TB total           | ✅ 14x minimum  |
| NVMe IOPS   | 500K    | Samsung 7450 (~1M+)    | ✅ Excellent    |
| Network     | 1 Gbps  | 100 Gbps               | ✅ 100x minimum |

**No hardware upgrades needed.** Proceed directly to RPC node setup.

### Alternative: External Geyser Providers (if not self-hosting)

If you prefer managed solutions over running your own node:

| Provider            | Service                 | Latency  | Price        |
| ------------------- | ----------------------- | -------- | ------------ |
| Triton (Triton One) | Yellowstone gRPC        | ~5-20ms  | ~$200-400/mo |
| Helius              | Geyser webhooks/streams | ~10-30ms | ~$200-300/mo |
| Shyft               | gRPC streaming          | ~10-30ms | ~$150-300/mo |

---

## Recommended Approach

### With current hardware:

→ **Run local RPC + Geyser** for lowest latency
→ Hardware already exceeds Solana validator requirements
→ No additional costs beyond power/bandwidth

---

## RPC Node Setup

### 1. Install Solana CLI

```bash
sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
```

### 2. Configure RPC Node (Non-Voting)

Create systemd service at `/etc/systemd/system/solana-rpc.service`:

```ini
[Unit]
Description=Solana RPC Node
After=network.target

[Service]
Type=simple
User=solana
ExecStart=/home/solana/.local/share/solana/install/active_release/bin/solana-validator \
    --identity /home/solana/validator-keypair.json \
    --no-voting \
    --ledger /mnt/ledger \
    --accounts /mnt/accounts \
    --rpc-port 8899 \
    --rpc-bind-address 0.0.0.0 \
    --private-rpc \
    --full-rpc-api \
    --limit-ledger-size 50000000 \
    --account-index program-id \
    --account-index spl-token-owner \
    --geyser-plugin-config /home/solana/geyser-config.json \
    --entrypoint entrypoint.mainnet-beta.solana.com:8001 \
    --entrypoint entrypoint2.mainnet-beta.solana.com:8001 \
    --known-validator 7Np41oeYqPefeNQEHSv1UDhYrehxin3NStELsSKCT4K2 \
    --known-validator GdnSyH3YtwcxFvQrVVJMm1JhTS4QVX7MFsX56uJLUfiZ \
    --expected-genesis-hash 5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d \
    --wal-recovery-mode skip_any_corrupted_record \
    --log /home/solana/solana-rpc.log

Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

### 3. Geyser Plugin Config

Create `/home/solana/geyser-config.json`:

```json
{
  "libpath": "/home/solana/yellowstone-grpc/target/release/libyellowstone_grpc_geyser.so",
  "log": {
    "level": "info"
  },
  "grpc": {
    "address": "0.0.0.0:10000",
    "max_decoding_message_size": "4_194_304",
    "snapshot_plugin_channel_capacity": "100_000_000",
    "snapshot_client_channel_capacity": "100_000_000",
    "channel_capacity": "1_000_000",
    "unary_concurrency_limit": 100,
    "unary_disabled": false
  },
  "prometheus": {
    "address": "0.0.0.0:8999"
  },
  "block_fail_action": "log"
}
```

### 4. Build Yellowstone Geyser Plugin

```bash
git clone https://github.com/rpcpool/yellowstone-grpc.git
cd yellowstone-grpc
cargo build --release
```

---

## Monitoring

### Key Metrics to Watch

```bash
# Check if node is caught up
solana catchup --our-localhost

# Check slot height
solana slot

# Memory usage (should stay under 80%)
free -h

# Disk I/O
iostat -x 1

# Geyser gRPC connections
ss -tuln | grep 10000
```

### Signs of Problems

| Symptom              | Cause               | Fix                                |
| -------------------- | ------------------- | ---------------------------------- |
| Swap usage > 0       | Not enough RAM      | Add more RAM                       |
| Falling behind slots | CPU/IOPS bottleneck | Check iostat, consider faster NVMe |
| Geyser disconnects   | Memory pressure     | Increase channel capacities or RAM |
| High disk latency    | NVMe throttling     | Check temps, consider better NVMe  |

---

## Next Steps

1. [ ] Check motherboard max RAM capacity: `sudo dmidecode -t memory | grep "Maximum Capacity"`
2. [ ] Decide on upgrade path (local upgrade vs cloud vs subscription)
3. [ ] If upgrading RAM: purchase compatible DDR5 ECC RDIMMs
4. [ ] Test with Solana testnet first before mainnet
5. [ ] Configure Geyser endpoint in tre config.json

---

## Integration with tre Bot

Once your validator is running with Geyser, configure tre for the unified pipeline:

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
    "skip_votes": true,
    "connect_timeout_ms": 10000,
    "request_timeout_ms": 30000
  },
  "local_rpc": {
    "enabled": true,
    "url": "http://localhost:8899",
    "ws_url": "ws://localhost:8900"
  }
}
```

Then run the bot:

```bash
./target/release/tre-arb-bot -c config.json
```

---

## ShredStream Proxy Setup

The ShredStream proxy connects to Jito's relay network for early transaction detection.

### Option 1: Run ShredStream Proxy Locally

```bash
# Clone and build the ShredStream proxy
git clone https://github.com/jito-foundation/shredstream-proxy.git
cd shredstream-proxy
cargo build --release

# Run the proxy (connects to Jito relay)
./target/release/shredstream-proxy \
    --grpc-bind-addr 127.0.0.1:10000 \
    --auth-keypair /path/to/jito-auth.json
```

### Option 2: Use Jito ShredStream Service

If you have Jito ShredStream access, configure the endpoint directly in your config.

### ShredStream + Geyser Integration

Both services should be accessible on localhost:

- **ShredStream proxy:** `http://127.0.0.1:10000` (for early TX detection)
- **Geyser plugin:** `http://localhost:10000` (for ground truth confirmation)

**Note:** If both use port 10000, configure different ports:

- ShredStream: `127.0.0.1:10001`
- Geyser: `127.0.0.1:10000`

---

## Optional: Direct Turbine UDP (Minimum Latency)

For even lower latency, the bot can receive shreds directly from the validator's Turbine UDP broadcast, bypassing the ShredStream proxy entirely.

### Latency Comparison

| Source             | Latency  | Description                                 |
| ------------------ | -------- | ------------------------------------------- |
| ShredStream Proxy  | ~5-10ms  | Validator → Jito Relay → Proxy → gRPC → Bot |
| Direct Turbine UDP | ~0.5-2ms | Validator TVU → UDP → Bot                   |

### Configuration

To enable direct Turbine reception, the bot needs to:

1. Bind to the TVU port (default 8001)
2. Receive raw shred packets
3. Perform FEC decoding and entry assembly

This is handled by the `turbine/` module in the bot codebase.

---

## Resources

- [Solana Validator Requirements](https://docs.solanalabs.com/operations/requirements)
- [Yellowstone gRPC Plugin](https://github.com/rpcpool/yellowstone-grpc)
- [Jito Geyser Plugin](https://github.com/jito-foundation/geyser-grpc-plugin)
- [Jito ShredStream Proxy](https://github.com/jito-foundation/shredstream-proxy)
- [Triton Yellowstone Service](https://triton.one/)
- [Helius Geyser Webhooks](https://docs.helius.dev/)
