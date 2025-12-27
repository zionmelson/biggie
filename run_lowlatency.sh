#!/bin/bash
# Low-latency launch script for tre-arb-bot

set -e

# Ensure CPU governor is set to performance
for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    echo performance | sudo tee $cpu > /dev/null 2>&1 || true
done

# Verify settings
echo "=== CPU Governor ==="
cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor

echo "=== Network Buffers ==="
sysctl net.core.rmem_max

echo "=== Huge Pages ==="
grep HugePages_Free /proc/meminfo

# Pin to CPUs 4-11 (leaving 0-3 for system), use NUMA node 0
echo "=== Starting bot pinned to CPUs 4-11 ==="

cd /home/ubuntu/biggie

# Run with CPU pinning and real-time priority (if available)
exec taskset -c 4-11 nice -n -20 ./target/release/tre-arb-bot --config ./config.json "$@"
