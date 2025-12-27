#!/usr/bin/env python3
"""
Prune pools.json by removing pools with low or zero TVL.
Queries on-chain vault balances and token prices to calculate TVL.
"""

import json
import asyncio
import aiohttp
from pathlib import Path
from typing import Optional
import time

# Configuration
MIN_TVL_USD = 1000  # Minimum TVL in USD to keep a pool
RPC_URL = "http://localhost:8899"  # Local validator - no rate limits
BATCH_SIZE = 100  # Number of accounts to fetch per RPC call
RATE_LIMIT_DELAY = 0  # No delay needed for local RPC

# Token price cache
price_cache: dict[str, Optional[float]] = {}

# Known token prices (hardcoded to avoid API issues)
TOKEN_PRICES = {
    # Stablecoins
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v": 1.0,    # USDC
    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB": 1.0,    # USDT
    # SOL and LSTs
    "So11111111111111111111111111111111111111112": 190.0,    # SOL
    "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So": 220.0,   # mSOL
    "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn": 210.0,  # jitoSOL
    "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1": 200.0,   # bSOL
    "jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v": 200.0,   # jupSOL
    # Major tokens
    "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN": 0.90,    # JUP
    "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263": 0.000030,  # BONK
    "WENWENvqqNya429ubCdR81ZmD69brwQaaBYY6p3LCpk": 0.00015,  # WEN
    "jtojtomepa8beP8AuQc6eXt5FriJwfFMwQx2v2f9mCL": 3.20,    # JTO
    "HZ1JovNiVvGrGNiiYvEozEVgZ58xaU3RKwX8eACQBCt3": 0.35,   # PYTH
    "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs": 0.65,   # SAMO
    "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr": 1.50,   # POPCAT
    "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN": 4.90,   # TRUMP
    "7dHbWXmci3dT8UFYWYZweBLXgycu7Y3iL6trKn1Y7ARj": 0.20,   # WIF
}

# SOL mint
SOL_MINT = "So11111111111111111111111111111111111111112"


async def get_multiple_accounts(session: aiohttp.ClientSession, addresses: list[str]) -> dict:
    """Fetch multiple token account balances in one RPC call."""
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getMultipleAccounts",
        "params": [
            addresses,
            {"encoding": "jsonParsed"}
        ]
    }
    
    try:
        async with session.post(RPC_URL, json=payload) as resp:
            if resp.status == 200:
                return await resp.json()
            else:
                print(f"RPC error: {resp.status}")
                return {}
    except Exception as e:
        print(f"Request error: {e}")
        return {}


async def get_token_prices(session: aiohttp.ClientSession, mints: list[str]) -> dict[str, float]:
    """Fetch token prices from CoinGecko."""
    prices = {}
    
    # Filter out already cached and stablecoins
    mints_to_fetch = [m for m in mints if m not in price_cache and m not in STABLECOINS]
    
    if not mints_to_fetch:
        return prices
    
    # CoinGecko has rate limits, batch requests
    batch_size = 50
    for i in range(0, len(mints_to_fetch), batch_size):
        batch = mints_to_fetch[i:i+batch_size]
        addresses = ",".join(batch)
        url = f"https://api.coingecko.com/api/v3/simple/token_price/solana?contract_addresses={addresses}&vs_currencies=usd"
        
        try:
            async with session.get(url) as resp:
                if resp.status == 200:
                    data = await resp.json()
                    for mint in batch:
                        if mint in data and "usd" in data[mint]:
                            prices[mint] = data[mint]["usd"]
                            price_cache[mint] = data[mint]["usd"]
                        else:
                            price_cache[mint] = None
                elif resp.status == 429:
                    print("Rate limited by CoinGecko, waiting...")
                    await asyncio.sleep(60)
                else:
                    print(f"CoinGecko error: {resp.status}")
        except Exception as e:
            print(f"Price fetch error: {e}")
        
        await asyncio.sleep(1)  # Rate limit
    
    return prices


def parse_token_balance(account_data: dict) -> tuple[Optional[int], Optional[int]]:
    """Parse token account data to get balance and decimals."""
    if not account_data or account_data.get("value") is None:
        return None, None
    
    try:
        parsed = account_data["value"]["data"]["parsed"]["info"]
        amount = int(parsed["tokenAmount"]["amount"])
        decimals = parsed["tokenAmount"]["decimals"]
        return amount, decimals
    except (KeyError, TypeError):
        return None, None


async def calculate_pool_tvl(
    session: aiohttp.ClientSession,
    pool: dict,
    vault_balances: dict[str, tuple[Optional[int], Optional[int]]]
) -> Optional[float]:
    """Calculate TVL for a single pool."""
    vault_a = pool["token_vault_a"]
    vault_b = pool["token_vault_b"]
    mint_a = pool["token_mint_a"]
    mint_b = pool["token_mint_b"]
    decimals_a = pool["decimals_a"]
    decimals_b = pool["decimals_b"]
    
    # Get vault balances
    balance_a, _ = vault_balances.get(vault_a, (None, None))
    balance_b, _ = vault_balances.get(vault_b, (None, None))
    
    if balance_a is None or balance_b is None:
        return None
    
    # Convert to UI amounts
    amount_a = balance_a / (10 ** decimals_a)
    amount_b = balance_b / (10 ** decimals_b)
    
    # Get prices from hardcoded TOKEN_PRICES
    price_a = TOKEN_PRICES.get(mint_a)
    price_b = TOKEN_PRICES.get(mint_b)
    
    # Calculate TVL
    tvl = 0.0
    if price_a is not None:
        tvl += amount_a * price_a
    if price_b is not None:
        tvl += amount_b * price_b
    
    # If we only have one price, estimate the other side (rough 50/50 assumption)
    if price_a is None and price_b is not None and amount_b > 0:
        tvl = tvl * 2  # Rough estimate
    elif price_b is None and price_a is not None and amount_a > 0:
        tvl = tvl * 2  # Rough estimate
    
    return tvl


async def main():
    pools_path = Path(__file__).parent.parent / "pools.json"
    
    print(f"Loading pools from {pools_path}...")
    with open(pools_path) as f:
        data = json.load(f)
    
    pools = data["pools"]
    print(f"Total pools: {len(pools)}")
    
    # Collect all vault addresses and token mints
    all_vaults = []
    all_mints = set()
    for pool in pools:
        all_vaults.append(pool["token_vault_a"])
        all_vaults.append(pool["token_vault_b"])
        all_mints.add(pool["token_mint_a"])
        all_mints.add(pool["token_mint_b"])
    
    print(f"Unique vaults to query: {len(all_vaults)}")
    print(f"Unique token mints: {len(all_mints)}")
    
    # Check which tokens have prices
    priced_mints = [m for m in all_mints if m in TOKEN_PRICES]
    unpriced_mints = [m for m in all_mints if m not in TOKEN_PRICES]
    print(f"Tokens with prices: {len(priced_mints)}")
    if unpriced_mints:
        print(f"Tokens without prices (will estimate): {unpriced_mints}")
    
    async with aiohttp.ClientSession() as session:
        # Fetch vault balances in batches
        print("\nFetching vault balances...")
        vault_balances: dict[str, tuple[Optional[int], Optional[int]]] = {}
        
        for i in range(0, len(all_vaults), BATCH_SIZE):
            batch = all_vaults[i:i+BATCH_SIZE]
            result = await get_multiple_accounts(session, batch)
            
            if "result" in result and "value" in result["result"]:
                for j, account in enumerate(result["result"]["value"]):
                    addr = batch[j]
                    balance, decimals = parse_token_balance({"value": account})
                    vault_balances[addr] = (balance, decimals)
            
            progress = min(i + BATCH_SIZE, len(all_vaults))
            print(f"  Fetched {progress}/{len(all_vaults)} vaults...", end="\r")
            await asyncio.sleep(RATE_LIMIT_DELAY)
        
        print(f"\nVault balances fetched: {len(vault_balances)}")
        
        # Calculate TVL for each pool
        print("\nCalculating TVL for each pool...")
        pool_tvls = []
        for pool in pools:
            tvl = await calculate_pool_tvl(session, pool, vault_balances)
            pool_tvls.append((pool, tvl))
        
        # Analyze results
        valid_pools = [(p, t) for p, t in pool_tvls if t is not None and t >= MIN_TVL_USD]
        zero_tvl = [(p, t) for p, t in pool_tvls if t is not None and t == 0]
        low_tvl = [(p, t) for p, t in pool_tvls if t is not None and 0 < t < MIN_TVL_USD]
        unknown_tvl = [(p, t) for p, t in pool_tvls if t is None]
        
        print(f"\n=== TVL Analysis ===")
        print(f"Pools with TVL >= ${MIN_TVL_USD:,}: {len(valid_pools)}")
        print(f"Pools with zero TVL: {len(zero_tvl)}")
        print(f"Pools with TVL < ${MIN_TVL_USD:,}: {len(low_tvl)}")
        print(f"Pools with unknown TVL: {len(unknown_tvl)}")
        
        # Show some examples of low TVL pools
        if low_tvl:
            print(f"\nExample low TVL pools:")
            for pool, tvl in sorted(low_tvl, key=lambda x: x[1] or 0)[:5]:
                print(f"  {pool['address'][:20]}... - ${tvl:,.2f} ({pool.get('_comment', 'N/A')})")
        
        # Create pruned pools list
        # Keep pools with good TVL + unknown TVL (to be safe)
        pruned_pools = [p for p, t in valid_pools]
        # Also keep unknown TVL pools (conservative approach)
        pruned_pools.extend([p for p, t in unknown_tvl])
        
        removed_count = len(pools) - len(pruned_pools)
        
        print(f"\n=== Pruning Results ===")
        print(f"Original pools: {len(pools)}")
        print(f"Removed (zero/low TVL): {removed_count}")
        print(f"Remaining pools: {len(pruned_pools)}")
        
        # Save pruned pools
        backup_path = pools_path.with_suffix(".json.bak")
        print(f"\nBacking up original to {backup_path}...")
        with open(backup_path, "w") as f:
            json.dump(data, f, indent=2)
        
        data["pools"] = pruned_pools
        print(f"Saving pruned pools to {pools_path}...")
        with open(pools_path, "w") as f:
            json.dump(data, f, indent=2)
        
        print("Done!")


if __name__ == "__main__":
    asyncio.run(main())
