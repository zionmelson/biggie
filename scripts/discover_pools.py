#!/usr/bin/env python3
"""
Discover all pools across DEXes for flash loan tokens.
Queries local Solana validator to find pools and build pools.json.
"""

import json
import base64
import struct
import requests
from typing import Dict, List, Optional, Tuple
from dataclasses import dataclass
from concurrent.futures import ThreadPoolExecutor, as_completed

RPC_URL = "http://localhost:8899"

# DEX Program IDs
PROGRAMS = {
    "RaydiumAmm": "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
    "RaydiumClmm": "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK",
    "OrcaWhirlpool": "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
    "MeteoraDlmm": "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "MeteoraDamm": "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
    "PancakeSwapClmm": "HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq",
}

# Flash loan tokens from config.json
FLASH_TOKENS = {
    "SOL": {"mint": "So11111111111111111111111111111111111111112", "decimals": 9},
    "USDC": {"mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "decimals": 6},
    "USDT": {"mint": "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "decimals": 6},
    "mSOL": {"mint": "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So", "decimals": 9},
    "JitoSOL": {"mint": "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn", "decimals": 9},
    "bSOL": {"mint": "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1", "decimals": 9},
    "jupSOL": {"mint": "jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v", "decimals": 9},
    "stSOL": {"mint": "7dHbWXmci3dT8UFYWYZweBLXgycu7Y3iL6trKn1Y7ARj", "decimals": 9},
    "ETH": {"mint": "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs", "decimals": 8},
    "JTO": {"mint": "jtojtomepa8beP8AuQc6eXt5FriJwfFMwQx2v2f9mCL", "decimals": 9},
    "JUP": {"mint": "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN", "decimals": 6},
    "PYTH": {"mint": "HZ1JovNiVvGrGNiiYvEozEVgZ58xaU3RKwX8eACQBCt3", "decimals": 6},
    "BONK": {"mint": "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263", "decimals": 5},
    "WEN": {"mint": "WENWENvqqNya429ubCdR81ZmD69brwQaaBYY6p3LCpk", "decimals": 5},
    "POPCAT": {"mint": "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr", "decimals": 9},
    "TRUMP": {"mint": "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN", "decimals": 6},
}

# Create mint to token name lookup
MINT_TO_TOKEN = {v["mint"]: k for k, v in FLASH_TOKENS.items()}
MINT_TO_DECIMALS = {v["mint"]: v["decimals"] for k, v in FLASH_TOKENS.items()}

def rpc_call(method: str, params: list) -> dict:
    """Make an RPC call to the local validator."""
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    }
    resp = requests.post(RPC_URL, json=payload, timeout=120)
    return resp.json()

def get_program_accounts(program_id: str, filters: list = None, data_slice: dict = None) -> list:
    """Get all accounts owned by a program with optional filters."""
    config = {"encoding": "base64"}
    if filters:
        config["filters"] = filters
    if data_slice:
        config["dataSlice"] = data_slice
    
    result = rpc_call("getProgramAccounts", [program_id, config])
    if "error" in result:
        print(f"Error fetching {program_id}: {result['error']}")
        return []
    return result.get("result", [])

def decode_pubkey(data: bytes, offset: int) -> str:
    """Decode a 32-byte pubkey from account data."""
    import base58
    return base58.b58encode(data[offset:offset+32]).decode()

def parse_raydium_amm(account: dict) -> Optional[dict]:
    """Parse Raydium AMM pool account."""
    try:
        data = base64.b64decode(account["account"]["data"][0])
        if len(data) < 752:
            return None
        
        # Raydium AMM layout offsets (from pool_discovery.rs)
        # status at 0, nonce at 8
        # token_coin_mint at offset around 400
        # token_pc_mint after that
        # We need to find the actual offsets - let me use known offsets
        
        # AMM V4 layout:
        # coin_vault at 336
        # pc_vault at 368
        # coin_mint at 400
        # pc_mint at 432
        coin_vault = decode_pubkey(data, 336)
        pc_vault = decode_pubkey(data, 368)
        coin_mint = decode_pubkey(data, 400)
        pc_mint = decode_pubkey(data, 432)
        
        # Check if both mints are in our flash token list
        if coin_mint not in MINT_TO_TOKEN or pc_mint not in MINT_TO_TOKEN:
            return None
        
        return {
            "address": account["pubkey"],
            "dex": "RaydiumAmm",
            "token_mint_a": coin_mint,
            "token_mint_b": pc_mint,
            "token_vault_a": coin_vault,
            "token_vault_b": pc_vault,
            "decimals_a": MINT_TO_DECIMALS[coin_mint],
            "decimals_b": MINT_TO_DECIMALS[pc_mint],
            "fee_bps": 25,
            "spacing": 0,
            "_comment": f"Raydium {MINT_TO_TOKEN[coin_mint]}/{MINT_TO_TOKEN[pc_mint]} AMM"
        }
    except Exception as e:
        return None

def parse_orca_whirlpool(account: dict) -> Optional[dict]:
    """Parse Orca Whirlpool pool account."""
    try:
        data = base64.b64decode(account["account"]["data"][0])
        if len(data) < 653:
            return None
        
        # Whirlpool layout from pool_discovery.rs:
        # WHIRLPOOLS_CONFIG: 8
        # WHIRLPOOL_BUMP: 40-44
        # TICK_SPACING: 41 (u16)
        # FEE_RATE: 45 (u16)
        # TOKEN_MINT_A: 101
        # TOKEN_VAULT_A: 133
        # TOKEN_MINT_B: 181
        # TOKEN_VAULT_B: 213
        # TICK_CURRENT_INDEX: 261-265
        
        tick_spacing = struct.unpack_from("<H", data, 41)[0]
        fee_rate = struct.unpack_from("<H", data, 45)[0]  # in hundredths of a bp
        
        token_mint_a = decode_pubkey(data, 101)
        token_vault_a = decode_pubkey(data, 133)
        token_mint_b = decode_pubkey(data, 181)
        token_vault_b = decode_pubkey(data, 213)
        current_tick = struct.unpack_from("<i", data, 261)[0]
        
        if token_mint_a not in MINT_TO_TOKEN or token_mint_b not in MINT_TO_TOKEN:
            return None
        
        fee_bps = fee_rate // 100  # Convert to bps
        
        return {
            "address": account["pubkey"],
            "dex": "OrcaWhirlpool",
            "token_mint_a": token_mint_a,
            "token_mint_b": token_mint_b,
            "token_vault_a": token_vault_a,
            "token_vault_b": token_vault_b,
            "decimals_a": MINT_TO_DECIMALS[token_mint_a],
            "decimals_b": MINT_TO_DECIMALS[token_mint_b],
            "fee_bps": fee_bps,
            "spacing": tick_spacing,
            "current_tick_or_bin": current_tick,
            "_comment": f"Orca {MINT_TO_TOKEN[token_mint_a]}/{MINT_TO_TOKEN[token_mint_b]} Whirlpool"
        }
    except Exception as e:
        return None

def parse_raydium_clmm(account: dict) -> Optional[dict]:
    """Parse Raydium CLMM pool account."""
    try:
        data = base64.b64decode(account["account"]["data"][0])
        if len(data) < 1544:
            return None
        
        # Raydium CLMM layout from pool_discovery.rs:
        # AMM_CONFIG: 8
        # TOKEN_MINT_0: 73
        # TOKEN_MINT_1: 105
        # TOKEN_VAULT_0: 137
        # TOKEN_VAULT_1: 169
        # TICK_SPACING: 257 (u16)
        # TICK_CURRENT: 265 (i32)
        # FEE_RATE: 269 (u32) - in hundredths of a bp
        
        amm_config = decode_pubkey(data, 8)
        token_mint_0 = decode_pubkey(data, 73)
        token_mint_1 = decode_pubkey(data, 105)
        token_vault_0 = decode_pubkey(data, 137)
        token_vault_1 = decode_pubkey(data, 169)
        tick_spacing = struct.unpack_from("<H", data, 257)[0]
        tick_current = struct.unpack_from("<i", data, 265)[0]
        fee_rate = struct.unpack_from("<I", data, 269)[0]
        
        if token_mint_0 not in MINT_TO_TOKEN or token_mint_1 not in MINT_TO_TOKEN:
            return None
        
        fee_bps = fee_rate // 100
        
        return {
            "address": account["pubkey"],
            "dex": "RaydiumClmm",
            "token_mint_a": token_mint_0,
            "token_mint_b": token_mint_1,
            "token_vault_a": token_vault_0,
            "token_vault_b": token_vault_1,
            "decimals_a": MINT_TO_DECIMALS[token_mint_0],
            "decimals_b": MINT_TO_DECIMALS[token_mint_1],
            "fee_bps": fee_bps,
            "spacing": tick_spacing,
            "current_tick_or_bin": tick_current,
            "amm_config": amm_config,
            "_comment": f"Raydium {MINT_TO_TOKEN[token_mint_0]}/{MINT_TO_TOKEN[token_mint_1]} CLMM"
        }
    except Exception as e:
        return None

def parse_meteora_dlmm(account: dict) -> Optional[dict]:
    """Parse Meteora DLMM LB pair account."""
    try:
        data = base64.b64decode(account["account"]["data"][0])
        # DLMM LbPair accounts are 904 bytes
        if len(data) != 904:
            return None
        
        # Meteora DLMM layout (discovered from on-chain analysis):
        # active_id (i32) @ 72
        # bin_step (u16) @ 80
        # Token X mint @ 88
        # Token Y mint @ 120
        # Reserve X @ 152
        # Reserve Y @ 184
        
        active_bin_id = struct.unpack_from("<i", data, 72)[0]
        bin_step = struct.unpack_from("<H", data, 80)[0]
        token_x_mint = decode_pubkey(data, 88)
        token_y_mint = decode_pubkey(data, 120)
        reserve_x = decode_pubkey(data, 152)
        reserve_y = decode_pubkey(data, 184)
        
        # base_factor at offset 8 for fee calculation
        base_factor = struct.unpack_from("<H", data, 8)[0]
        fee_bps = (base_factor * bin_step) // 10000 if bin_step > 0 else 25
        if fee_bps == 0:
            fee_bps = bin_step  # Fallback
        
        if token_x_mint not in MINT_TO_TOKEN or token_y_mint not in MINT_TO_TOKEN:
            return None
        
        return {
            "address": account["pubkey"],
            "dex": "MeteoraDlmm",
            "token_mint_a": token_x_mint,
            "token_mint_b": token_y_mint,
            "token_vault_a": reserve_x,
            "token_vault_b": reserve_y,
            "decimals_a": MINT_TO_DECIMALS[token_x_mint],
            "decimals_b": MINT_TO_DECIMALS[token_y_mint],
            "fee_bps": fee_bps,
            "spacing": bin_step,
            "current_tick_or_bin": active_bin_id,
            "_comment": f"Meteora {MINT_TO_TOKEN[token_x_mint]}/{MINT_TO_TOKEN[token_y_mint]} DLMM"
        }
    except Exception as e:
        return None

def parse_meteora_damm(account: dict) -> Optional[dict]:
    """Parse Meteora DAMM (CP-AMM) pool account."""
    try:
        data = base64.b64decode(account["account"]["data"][0])
        # DAMM pools are 1112 bytes
        if len(data) != 1112:
            return None
        
        # DAMM layout (discovered from on-chain analysis):
        # Token mint A @ 168
        # Token mint B @ 200
        # Token vault A @ 232
        # Token vault B @ 264
        
        token_a_mint = decode_pubkey(data, 168)
        token_b_mint = decode_pubkey(data, 200)
        token_a_vault = decode_pubkey(data, 232)
        token_b_vault = decode_pubkey(data, 264)
        
        if token_a_mint not in MINT_TO_TOKEN or token_b_mint not in MINT_TO_TOKEN:
            return None
        
        return {
            "address": account["pubkey"],
            "dex": "MeteoraDamm",
            "token_mint_a": token_a_mint,
            "token_mint_b": token_b_mint,
            "token_vault_a": token_a_vault,
            "token_vault_b": token_b_vault,
            "decimals_a": MINT_TO_DECIMALS[token_a_mint],
            "decimals_b": MINT_TO_DECIMALS[token_b_mint],
            "fee_bps": 25,  # Default, actual fee may vary
            "spacing": 0,
            "_comment": f"Meteora {MINT_TO_TOKEN[token_a_mint]}/{MINT_TO_TOKEN[token_b_mint]} DAMM"
        }
    except Exception as e:
        return None

def parse_pancakeswap_clmm(account: dict) -> Optional[dict]:
    """Parse PancakeSwap CLMM pool account - similar to Raydium CLMM."""
    try:
        data = base64.b64decode(account["account"]["data"][0])
        if len(data) < 500:
            return None
        
        # PancakeSwap CLMM has similar layout to Raydium CLMM
        amm_config = decode_pubkey(data, 8)
        token_mint_0 = decode_pubkey(data, 73)
        token_mint_1 = decode_pubkey(data, 105)
        token_vault_0 = decode_pubkey(data, 137)
        token_vault_1 = decode_pubkey(data, 169)
        tick_spacing = struct.unpack_from("<H", data, 257)[0]
        tick_current = struct.unpack_from("<i", data, 265)[0]
        
        if token_mint_0 not in MINT_TO_TOKEN or token_mint_1 not in MINT_TO_TOKEN:
            return None
        
        return {
            "address": account["pubkey"],
            "dex": "PancakeSwapClmm",
            "token_mint_a": token_mint_0,
            "token_mint_b": token_mint_1,
            "token_vault_a": token_vault_0,
            "token_vault_b": token_vault_1,
            "decimals_a": MINT_TO_DECIMALS[token_mint_0],
            "decimals_b": MINT_TO_DECIMALS[token_mint_1],
            "fee_bps": 25,  # Default
            "spacing": tick_spacing,
            "current_tick_or_bin": tick_current,
            "amm_config": amm_config,
            "_comment": f"PancakeSwap {MINT_TO_TOKEN[token_mint_0]}/{MINT_TO_TOKEN[token_mint_1]} CLMM"
        }
    except Exception as e:
        return None

PARSERS = {
    "RaydiumAmm": parse_raydium_amm,
    "RaydiumClmm": parse_raydium_clmm,
    "OrcaWhirlpool": parse_orca_whirlpool,
    "MeteoraDlmm": parse_meteora_dlmm,
    "MeteoraDamm": parse_meteora_damm,
    "PancakeSwapClmm": parse_pancakeswap_clmm,
}

def discover_pools_for_dex(dex_name: str, program_id: str) -> List[dict]:
    """Discover all pools for a specific DEX."""
    print(f"Fetching {dex_name} pools from {program_id}...")
    
    accounts = get_program_accounts(program_id)
    print(f"  Found {len(accounts)} total accounts")
    
    parser = PARSERS.get(dex_name)
    if not parser:
        print(f"  No parser for {dex_name}")
        return []
    
    pools = []
    for account in accounts:
        pool = parser(account)
        if pool:
            pools.append(pool)
    
    print(f"  Matched {len(pools)} pools with flash loan tokens")
    return pools

def main():
    import base58  # Install with: pip install base58
    
    all_pools = []
    
    for dex_name, program_id in PROGRAMS.items():
        pools = discover_pools_for_dex(dex_name, program_id)
        all_pools.extend(pools)
    
    # Sort pools by DEX type and token pair
    all_pools.sort(key=lambda p: (p["dex"], p["_comment"]))
    
    # Remove duplicates (same address)
    seen = set()
    unique_pools = []
    for pool in all_pools:
        if pool["address"] not in seen:
            seen.add(pool["address"])
            unique_pools.append(pool)
    
    print(f"\nTotal unique pools found: {len(unique_pools)}")
    
    # Write to pools.json
    output = {"pools": unique_pools}
    with open("pools.json", "w") as f:
        json.dump(output, f, indent=2)
    
    print("Wrote pools.json")
    
    # Also print summary by DEX
    from collections import Counter
    dex_counts = Counter(p["dex"] for p in unique_pools)
    print("\nPools by DEX:")
    for dex, count in sorted(dex_counts.items()):
        print(f"  {dex}: {count}")

if __name__ == "__main__":
    main()
