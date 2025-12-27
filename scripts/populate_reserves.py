#!/usr/bin/env python3
"""
Fetch initial vault reserves for pools.json

Geyser only sends updates when accounts change, not initial state.
This script fetches current vault balances and adds them to pools.json
so the bot starts with accurate reserve data.
"""

import json
import asyncio
import aiohttp
import struct
from pathlib import Path
from typing import Optional

RPC_URL = "http://localhost:8899"
POOLS_FILE = Path(__file__).parent.parent / "pools.json"


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
    import base64
    try:
        data = base64.b64decode(data_b64)
        if len(data) >= 72:
            amount = struct.unpack('<Q', data[64:72])[0]
            return amount
    except Exception as e:
        print(f"Parse error: {e}")
    return None


async def main():
    # Load pools.json
    with open(POOLS_FILE) as f:
        pools_data = json.load(f)
    
    pools = pools_data["pools"]
    print(f"Loaded {len(pools)} pools")
    
    # Collect all vault addresses
    vault_to_pool = {}  # vault_address -> (pool_index, is_base)
    vault_addresses = []
    
    for i, pool in enumerate(pools):
        vault_a = pool["token_vault_a"]
        vault_b = pool["token_vault_b"]
        vault_to_pool[vault_a] = (i, True)  # base vault
        vault_to_pool[vault_b] = (i, False)  # quote vault
        vault_addresses.append(vault_a)
        vault_addresses.append(vault_b)
    
    print(f"Fetching {len(vault_addresses)} vault accounts...")
    
    async with aiohttp.ClientSession() as session:
        accounts = await get_multiple_accounts(session, vault_addresses)
    
    # Parse balances and update pools
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
