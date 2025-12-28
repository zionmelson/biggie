#!/usr/bin/env python3
"""
Fetch CLMM metadata for pools.json

This script fetches CLMM-specific data (observation_state, sqrt_price, 
current_tick) so the bot has the required PDAs for swap instructions.

Note: Vault reserves are NOT fetched here - they are stale immediately
and Geyser provides live updates at runtime.
"""

import json
import asyncio
import aiohttp
import struct
import base64
from pathlib import Path
from typing import Optional

RPC_URL = "http://localhost:8899"
POOLS_FILE = Path(__file__).parent.parent / "pools.json"

# Raydium CLMM pool account layout offsets
RAYDIUM_CLMM_SQRT_PRICE = 253     # u128 (16 bytes)
RAYDIUM_CLMM_TICK_CURRENT = 269  # i32 (4 bytes)
RAYDIUM_CLMM_OBSERVATION = 201   # Pubkey (32 bytes)
RAYDIUM_CLMM_AMM_CONFIG = 8      # Pubkey (32 bytes, after 8-byte discriminator)


async def get_multiple_accounts(session: aiohttp.ClientSession, addresses: list[str]) -> list[Optional[dict]]:
    """Fetch multiple accounts in batches of 100."""
    results = []
    batch_size = 100
    
    for i in range(0, len(addresses), batch_size):
        batch = addresses[i:i+batch_size]
        payload = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getMultipleAccounts",
            "params": [
                batch,
                {"encoding": "base64"}
            ]
        }
        
        try:
            async with session.post(RPC_URL, json=payload) as resp:
                if resp.status == 200:
                    data = await resp.json()
                    if "result" in data and "value" in data["result"]:
                        results.extend(data["result"]["value"])
                    else:
                        print(f"Unexpected response: {data}")
                        results.extend([None] * len(batch))
                else:
                    print(f"RPC error: {resp.status}")
                    results.extend([None] * len(batch))
        except Exception as e:
            print(f"Request error: {e}")
            results.extend([None] * len(batch))
        
        # Small delay between batches to avoid overwhelming RPC
        if i + batch_size < len(addresses):
            await asyncio.sleep(0.1)
    
    return results


def parse_raydium_clmm_pool(data_b64: str) -> Optional[dict]:
    """Parse Raydium CLMM pool account to extract observation_state, sqrt_price, tick.
    
    Returns dict with:
    - observation_state: Pubkey as base58 string
    - sqrt_price: u128 value
    - current_tick_or_bin: i32 value
    """
    try:
        data = base64.b64decode(data_b64)
        # Minimum size check (observation at 201 + 32 bytes, tick at 269 + 4 bytes)
        if len(data) < 273:
            return None
        
        # Parse observation_state (Pubkey at offset 201)
        observation_bytes = data[RAYDIUM_CLMM_OBSERVATION:RAYDIUM_CLMM_OBSERVATION + 32]
        # Convert to base58
        import base58
        observation_state = base58.b58encode(observation_bytes).decode('ascii')
        
        # Parse sqrt_price (u128 at offset 253)
        sqrt_price = int.from_bytes(data[RAYDIUM_CLMM_SQRT_PRICE:RAYDIUM_CLMM_SQRT_PRICE + 16], 'little')
        
        # Parse current_tick (i32 at offset 269)
        current_tick = struct.unpack('<i', data[RAYDIUM_CLMM_TICK_CURRENT:RAYDIUM_CLMM_TICK_CURRENT + 4])[0]
        
        return {
            "observation_state": observation_state,
            "sqrt_price": sqrt_price,
            "current_tick_or_bin": current_tick,
        }
    except Exception as e:
        print(f"Parse CLMM error: {e}")
    return None


async def main():
    # Load pools.json
    with open(POOLS_FILE) as f:
        pools_data = json.load(f)
    
    pools = pools_data["pools"]
    print(f"Loaded {len(pools)} pools")
    
    # Track CLMM pools that need observation_state
    clmm_pools = []  # (pool_index, pool_address)
    
    for i, pool in enumerate(pools):
        # Track CLMM pools that need observation_state
        dex = pool.get("dex", "")
        if dex in ("RaydiumClmm", "raydium_clmm") and not pool.get("observation_state"):
            clmm_pools.append((i, pool["address"]))
    
    print(f"Found {len(clmm_pools)} Raydium CLMM pools needing observation_state")
    
    if not clmm_pools:
        print("No CLMM pools to update")
        return
    
    async with aiohttp.ClientSession() as session:
        # Fetch CLMM pool accounts
        clmm_addresses = [addr for _, addr in clmm_pools]
        clmm_accounts = await get_multiple_accounts(session, clmm_addresses)
    
    # Parse CLMM pool accounts for observation_state
    clmm_updated = 0
    for (pool_idx, pool_addr), account in zip(clmm_pools, clmm_accounts):
        if account is None:
            continue
        
        if "data" not in account or not account["data"]:
            continue
        
        data_b64 = account["data"][0]
        clmm_data = parse_raydium_clmm_pool(data_b64)
        
        if clmm_data:
            pools[pool_idx]["observation_state"] = clmm_data["observation_state"]
            pools[pool_idx]["current_tick_or_bin"] = clmm_data["current_tick_or_bin"]
            # sqrt_price is stored as string to avoid JSON precision issues with large u128
            pools[pool_idx]["sqrt_price"] = str(clmm_data["sqrt_price"])
            clmm_updated += 1
            print(f"  Updated CLMM pool {pool_addr[:8]}: tick={clmm_data['current_tick_or_bin']}")
    
    print(f"Updated {clmm_updated} CLMM pools with observation_state")
    
    # Write back
    with open(POOLS_FILE, 'w') as f:
        json.dump(pools_data, f, indent=2)
    
    print(f"Wrote updated pools.json")


if __name__ == "__main__":
    asyncio.run(main())
