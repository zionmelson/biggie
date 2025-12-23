# RPC Node + Geyser Setup for MEV Bot

This document outlines hardware requirements and upgrade paths for running a Solana RPC node (non-voting) with Geyser plugin for MEV bot operations.

## Current Machine Specs

| Component | Current | Status |
|-----------|---------|--------|
| CPU | AMD EPYC 4244P, 6 cores / 12 threads, 5.1GHz boost | ⚠️ Marginal |
| RAM | 64 GB | ❌ Insufficient |
| Storage | 2x 894GB NVMe (Micron TLC) | ⚠️ Marginal |
| Network | 10 Gbps | ✅ Good |

---

## Hardware Requirements: Priority Ranking

For MEV bot use case (RPC + Geyser, no voting):

| Priority | Component | Why It Matters | Minimum | Sweet Spot |
|----------|-----------|----------------|---------|------------|
| **#1** | **RAM** | Accounts DB is memory-mapped. Too little = swap death spiral | 256 GB | 384-512 GB |
| **#2** | **NVMe IOPS** | Ledger writes, accounts I/O. Random 4K matters most | 500K IOPS | 1M+ IOPS |
| **#3** | **NVMe Size** | Ledger + accounts + snapshots | 2 TB | 4 TB |
| **#4** | **Network** | Gossip, block propagation | 1 Gbps | 10 Gbps |
| **#5** | **CPU** | Replay, signature verification | 8 cores | 16 cores |

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

**64GB = instant death.** Node will swap, fall behind, and never catch up.

---

## What You Can Skip (Non-Voting vs Voting)

| Component | Voting Validator | RPC Node (Non-Voting) |
|-----------|------------------|----------------------|
| Dedicated PoH core | Critical | Not needed |
| TPU/leader threads | Critical | Not needed |
| 99.9% uptime | Required for rewards | Nice to have |
| Redundant NVMe RAID | Required | Single disk OK |
| CPU cores | 16+ required | 8-12 acceptable |
| Hot standby | Often needed | Not needed |

---

## Upgrade Path Options

### Option A: Upgrade Current Machine

**Minimum Viable Upgrade (~$750-1100)**

| Upgrade | Specification | Est. Cost |
|---------|---------------|-----------|
| RAM → 256GB | 8x 32GB DDR5 ECC RDIMM | $600-900 |
| NVMe → 2TB | Samsung 990 Pro or Crucial T700 (Gen4) | $150-200 |

**Comfortable Upgrade (~$1200-1800)**

| Upgrade | Specification | Est. Cost |
|---------|---------------|-----------|
| RAM → 384GB | 12x 32GB DDR5 ECC RDIMM | $900-1300 |
| NVMe → 4TB | 2x 2TB Gen4 NVMe | $300-400 |

**Notes:**
- Check motherboard max RAM capacity before purchasing
- Your CPU (6-core EPYC @ 5GHz) is acceptable for RPC replay
- Existing 10Gbps network is sufficient

### Option B: Cloud Bare Metal (~$300-500/month)

Pre-configured Solana-ready instances:

| Provider | Instance | Specs | Price |
|----------|----------|-------|-------|
| Latitude.sh | c3.large.x86 | 32-core, 256GB, 2TB NVMe | ~$350/mo |
| Teraswitch | Solana-optimized | 32-core, 512GB, 4TB NVMe | ~$450/mo |
| Vultr | Bare Metal | 24-core, 256GB, 2TB NVMe | ~$400/mo |

**Pros:** No hardware hassle, quick setup, can scale
**Cons:** Ongoing monthly cost, slightly higher latency than local

### Option C: Yellowstone gRPC Subscription (~$200-400/month)

Skip the node entirely, subscribe to Geyser data:

| Provider | Service | Latency | Price |
|----------|---------|---------|-------|
| Triton (Triton One) | Yellowstone gRPC | ~5-20ms | ~$200-400/mo |
| Helius | Geyser webhooks/streams | ~10-30ms | ~$200-300/mo |
| Shyft | gRPC streaming | ~10-30ms | ~$150-300/mo |

**Pros:** Zero hardware changes, immediate setup
**Cons:** 10-20ms additional latency vs local Geyser, ongoing cost

---

## Cost-Benefit Analysis

| Approach | Upfront Cost | Monthly Cost | Latency | Effort |
|----------|--------------|--------------|---------|--------|
| Upgrade RAM | $600-1300 | ~$50 (power) | Lowest | Medium |
| Cloud bare metal | $0 | $300-500 | Low | Low |
| Yellowstone subscription | $0 | $200-400 | Medium | Lowest |
| Status quo (Shredstream only) | $0 | $0 | N/A | None |

---

## Recommended Approach

### If MEV bot is experimental/learning:
→ **Keep using Shredstream + RPC simulation** (current setup)
→ Add Yellowstone subscription later if needed

### If MEV bot is generating revenue:
→ **Upgrade RAM to 256GB** ($600-900 one-time)
→ Run local RPC + Geyser for lowest latency
→ Best ROI if you plan to run for 3+ months

### If you want lowest friction:
→ **Yellowstone subscription** ($200-400/month)
→ Same data as local Geyser, just remote
→ Can always switch to self-hosted later

---

## RPC Node Setup (After Hardware Upgrade)

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

| Symptom | Cause | Fix |
|---------|-------|-----|
| Swap usage > 0 | Not enough RAM | Add more RAM |
| Falling behind slots | CPU/IOPS bottleneck | Check iostat, consider faster NVMe |
| Geyser disconnects | Memory pressure | Increase channel capacities or RAM |
| High disk latency | NVMe throttling | Check temps, consider better NVMe |

---

## Next Steps

1. [ ] Check motherboard max RAM capacity: `sudo dmidecode -t memory | grep "Maximum Capacity"`
2. [ ] Decide on upgrade path (local upgrade vs cloud vs subscription)
3. [ ] If upgrading RAM: purchase compatible DDR5 ECC RDIMMs
4. [ ] Test with Solana testnet first before mainnet
5. [ ] Integrate Geyser gRPC with MEV bot pipeline

---

## Resources

- [Solana Validator Requirements](https://docs.solanalabs.com/operations/requirements)
- [Yellowstone gRPC Plugin](https://github.com/rpcpool/yellowstone-grpc)
- [Jito Geyser Plugin](https://github.com/jito-foundation/geyser-grpc-plugin)
- [Triton Yellowstone Service](https://triton.one/)
- [Helius Geyser Webhooks](https://docs.helius.dev/)
