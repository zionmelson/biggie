#!/usr/bin/env python3
"""
Fetch initial vault reserves and CLMM metadata for pools.json

Geyser only sends updates when accounts change, not initial state.
This script fetches current vault balances and CLMM-specific data
(observation_state, sqrt_price, current_tick) so the bot starts with
accurate data for arbitrage calculations.
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


def parse_token_account_balance(data_b64: str) -> Optional[int]:
    """Parse SPL token account to extract balance.
    
    Token account layout:
    - bytes 0-32: mint
    - bytes 32-64: owner
    - bytes 64-72: amount (u64 little-endian)
    """
    try:
        data = base64.b64decode(data_b64)
        if len(data) >= 72:
            amount = struct.unpack('<Q', data[64:72])[0]
            return amount
    except Exception as e:
        print(f"Parse error: {e}")
    return None


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
    
    # Collect all vault addresses and CLMM pool addresses
    vault_to_pool = {}  # vault_address -> (pool_index, is_base)
    vault_addresses = []
    clmm_pools = []  # (pool_index, pool_address)
    
    for i, pool in enumerate(pools):
        vault_a = pool["token_vault_a"]
        vault_b = pool["token_vault_b"]
        vault_to_pool[vault_a] = (i, True)  # base vault
        vault_to_pool[vault_b] = (i, False)  # quote vault
        vault_addresses.append(vault_a)
        vault_addresses.append(vault_b)
        
        # Track CLMM pools that need observation_state
        dex = pool.get("dex", "")
        if dex in ("RaydiumClmm", "raydium_clmm") and not pool.get("observation_state"):
            clmm_pools.append((i, pool["address"]))
    
    print(f"Fetching {len(vault_addresses)} vault accounts...")
    print(f"Found {len(clmm_pools)} Raydium CLMM pools needing observation_state")
    
    async with aiohttp.ClientSession() as session:
        # Fetch vault accounts
        accounts = await get_multiple_accounts(session, vault_addresses)
        
        # Fetch CLMM pool accounts
        if clmm_pools:
            clmm_addresses = [addr for _, addr in clmm_pools]
            clmm_accounts = await get_multiple_accounts(session, clmm_addresses)
        else:
            clmm_accounts = []
    
    # Parse vault balances and update pools
    updated = 0
    for addr, account in zip(vault_addresses, accounts):
        if account is None:
            continue
        
        if "data" not in account or not account["data"]:
            continue
        
        # data is [base64_string, encoding]
        data_b64 = account["data"][0]
        balance = parse_token_account_balance(data_b64)
        
        if balance is not None:
            pool_idx, is_base = vault_to_pool[addr]
            if is_base:
                pools[pool_idx]["base_reserve"] = balance
            else:
                pools[pool_idx]["quote_reserve"] = balance
            updated += 1
    
    print(f"Updated {updated} vault balances ({updated // 2} pools)")
    
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
    
    # Count pools with both reserves
    complete = 0
    for pool in pools:
        if pool.get("base_reserve", 0) > 0 and pool.get("quote_reserve", 0) > 0:
            complete += 1
    
    print(f"Pools with complete reserve data: {complete}/{len(pools)}")
    
    # Write back
    with open(POOLS_FILE, 'w') as f:
        json.dump(pools_data, f, indent=2)
    
    print(f"Wrote updated pools.json")


if __name__ == "__main__":
    asyncio.run(main())
