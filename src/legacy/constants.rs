/// Shared constants used across the arbitrage bot.
use solana_sdk::pubkey::Pubkey;

// =============================================================================
// TOKEN MINTS (as Pubkey)
// =============================================================================

/// Native SOL mint (wrapped SOL)
pub const WSOL_MINT: Pubkey = solana_sdk::pubkey!("So11111111111111111111111111111111111111112");
/// USDC mint
pub const USDC_MINT: Pubkey = solana_sdk::pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
/// USDT mint
pub const USDT_MINT: Pubkey = solana_sdk::pubkey!("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB");
/// mSOL mint
pub const MSOL_MINT: Pubkey = solana_sdk::pubkey!("mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So");
/// jitoSOL mint
pub const JITOSOL_MINT: Pubkey =
    solana_sdk::pubkey!("J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn");

// =============================================================================
// TOKEN MINTS (as &str for parsing/comparison)
// =============================================================================

pub const WSOL_MINT_STR: &str = "So11111111111111111111111111111111111111112";
pub const USDC_MINT_STR: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const USDT_MINT_STR: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
pub const MSOL_MINT_STR: &str = "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So";
pub const JITOSOL_MINT_STR: &str = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";

// =============================================================================
// PROGRAM IDS
// =============================================================================

/// Solend program ID
pub const SOLEND_PROGRAM: Pubkey =
    solana_sdk::pubkey!("So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo");
/// Token program (SPL Token)
pub const TOKEN_PROGRAM: Pubkey =
    solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// Token-2022 program (SPL Token Extensions)
pub const TOKEN_2022_PROGRAM: Pubkey =
    solana_sdk::pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
/// Associated Token program
pub const ATA_PROGRAM: Pubkey = solana_sdk::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
/// System program
pub const SYSTEM_PROGRAM: Pubkey = solana_sdk::pubkey!("11111111111111111111111111111111");
/// SPL Memo program (required for Meteora DLMM swaps)
pub const SPL_MEMO_PROGRAM: Pubkey =
    solana_sdk::pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

// =============================================================================
// TOKEN-2022 SUPPORT
// =============================================================================

/// Maximum transfer fee in basis points for Token-2022 tokens
pub const MAX_TOKEN_2022_TRANSFER_FEE_BPS: u64 = 100; // 1% max expected

/// ATA rent-exemption cost in lamports (for balance checks)
pub const ATA_RENT_LAMPORTS: u64 = 2_039_280;

/// Minimum SOL balance required to execute an arb bundle
/// Covers: 2x ATA rent + tip + tx fee + buffer
pub const MIN_PAYER_BALANCE_LAMPORTS: u64 = 5_000_000; // 0.005 SOL

// =============================================================================
// FEE CONSTANTS (in basis points, 1 bp = 0.01%)
// =============================================================================

/// Solend flash loan fee: 0.3% = 30 bps
pub const SOLEND_FLASH_LOAN_BPS: u64 = 30;
/// Average DEX swap fee: ~0.25% = 25 bps (varies by pool)
pub const AVG_SWAP_FEE_BPS: u64 = 25;
/// Jito tip minimum in lamports (absolute floor)
pub const MIN_TIP_LAMPORTS: u64 = 1_000;
/// Default base tip - 75th percentile of recent landed tips
pub const BASE_TIP_LAMPORTS: u64 = 10_000;
/// Percentage of profit to share as tip (50% = aggressive bidding)
pub const TIP_PROFIT_SHARE_BPS: u64 = 5000;
/// Maximum tip cap as percentage of profit (keep at least 20% profit)
pub const MAX_TIP_PROFIT_BPS: u64 = 8000;

// =============================================================================
// CLMM PRICE LIMITS
// =============================================================================

/// Minimum sqrt_price for Orca Whirlpool (Q64.64 format)
/// From whirlpools: MIN_SQRT_PRICE_X64 = 4295048016
pub const ORCA_MIN_SQRT_PRICE: u128 = 4295048016;
/// Maximum sqrt_price for Orca Whirlpool (Q64.64 format)
/// From whirlpools: MAX_SQRT_PRICE_X64 = 79226673515401279992447579055
pub const ORCA_MAX_SQRT_PRICE: u128 = 79226673515401279992447579055;

/// Minimum sqrt_price for Raydium CLMM (Q64.64 format)
/// From raydium-clmm: MIN_SQRT_PRICE_X64 = 4295048016
pub const RAYDIUM_CLMM_MIN_SQRT_PRICE: u128 = 4295048016;
/// Maximum sqrt_price for Raydium CLMM (Q64.64 format)
/// From raydium-clmm: MAX_SQRT_PRICE_X64 = 79226673515401279992447579055
pub const RAYDIUM_CLMM_MAX_SQRT_PRICE: u128 = 79226673515401279992447579055;

/// Minimum sqrt_price for PancakeSwap CLMM (same as Raydium)
pub const PANCAKE_CLMM_MIN_SQRT_PRICE: u128 = 4295048016;
/// Maximum sqrt_price for PancakeSwap CLMM (same as Raydium)
pub const PANCAKE_CLMM_MAX_SQRT_PRICE: u128 = 79226673515401279992447579055;

// =============================================================================
// ARB EVALUATION THRESHOLDS
// =============================================================================

/// Minimum expected profit in lamports to submit a bundle
/// Must be >= 2x typical P75 tip floor (~10k) to be profitable after tipping
/// Set higher to avoid wasting bundles on unprofitable opportunities
pub const MIN_PROFIT_LAMPORTS: u64 = 25_000; // 0.000025 SOL - covers P75 tip + keeper

/// Minimum trade size in lamports (0.001 SOL)
/// Trades smaller than this are not worth the compute/tip overhead
pub const MIN_TRADE_LAMPORTS: u64 = 1_000_000;

// =============================================================================
// BUNDLE BUILDER DEFAULTS
// =============================================================================

/// Default compute units to request per arb transaction
pub const DEFAULT_COMPUTE_UNITS: u32 = 400_000;
/// Default compute unit price in micro-lamports
pub const DEFAULT_COMPUTE_UNIT_PRICE: u64 = 1_000;
/// Default base tip in lamports (minimum tip regardless of profit)
pub const DEFAULT_BASE_TIP_LAMPORTS: u64 = 10_000;
/// Default tip as percentage of profit (50% = 5000 bps)
pub const DEFAULT_TIP_PROFIT_SHARE_BPS: u64 = 5_000;
/// Default max tip as percentage of profit (80% = 8000 bps)
pub const DEFAULT_MAX_TIP_PROFIT_BPS: u64 = 8_000;
/// Default slippage tolerance (2% = 200 bps)
/// Increased from 1% to handle price volatility between detection and execution
pub const DEFAULT_SLIPPAGE_BPS: u64 = 200;
/// Default multiplier over tip floor for competitive bidding
pub const DEFAULT_TIP_FLOOR_MULTIPLIER: f64 = 2.0;
/// Default bundle submission timeout in seconds
pub const DEFAULT_SUBMISSION_TIMEOUT_SECS: u64 = 5;
/// Default minimum interval between submissions in milliseconds
pub const DEFAULT_MIN_SUBMISSION_INTERVAL_MS: u64 = 200;

// =============================================================================
// TIP FLOOR MONITOR DEFAULTS
// =============================================================================

/// Default P50 tip floor in lamports (fallback before first API fetch)
pub const DEFAULT_TIP_P50_LAMPORTS: u64 = 1_000;
/// Default P75 tip floor in lamports
pub const DEFAULT_TIP_P75_LAMPORTS: u64 = 2_500;
/// Default P95 tip floor in lamports
pub const DEFAULT_TIP_P95_LAMPORTS: u64 = 5_000;
/// Default P99 tip floor in lamports
pub const DEFAULT_TIP_P99_LAMPORTS: u64 = 150_000;
/// Tip floor cache staleness threshold in seconds
pub const TIP_FLOOR_STALE_SECS: u64 = 30;
/// Tip floor polling interval in seconds
pub const TIP_FLOOR_POLL_SECS: u64 = 5;

// =============================================================================
// JITO CLIENT TIMEOUTS
// =============================================================================

/// Token refresh buffer - refresh tokens this many seconds before expiry
pub const JITO_TOKEN_REFRESH_BUFFER_SECS: u64 = 30;
/// gRPC request timeout in seconds
pub const JITO_GRPC_TIMEOUT_SECS: u64 = 30;
/// gRPC connection timeout in seconds
pub const JITO_CONNECT_TIMEOUT_SECS: u64 = 10;

// =============================================================================
// FEE CALCULATION HELPERS
// =============================================================================

/// Calculate flash loan fee in base units
#[inline]
pub fn calculate_flash_loan_fee(borrow_amount: u64) -> u64 {
    (borrow_amount as u128 * SOLEND_FLASH_LOAN_BPS as u128 / 10_000) as u64
}

/// Calculate minimum amount needed to repay flash loan (principal + fee)
#[inline]
pub fn flash_loan_repay_amount(borrow_amount: u64) -> u64 {
    borrow_amount.saturating_add(calculate_flash_loan_fee(borrow_amount))
}

/// Calculate minimum output needed to cover flash loan + buffer for safety
#[inline]
pub fn min_output_for_flash_loan(borrow_amount: u64, buffer_bps: u64) -> u64 {
    let repay = flash_loan_repay_amount(borrow_amount);
    repay.saturating_add((borrow_amount as u128 * buffer_bps as u128 / 10_000) as u64)
}

// =============================================================================
// ANCHOR MINTS
// =============================================================================
// NOTE: Anchor mints are now derived dynamically from configured flash loan reserves
// at startup in UnifiedArbPipeline::new(). This ensures we only attempt flash loans
// for tokens where Solend reserves are configured in config.json.

// =============================================================================
// SUPPORTED MINTS
// =============================================================================

/// Token mints for which we have ATAs - used for upstream filtering
/// These are the tokens we can actually trade
pub const SUPPORTED_MINT_STRS: &[&str] = &[
    // Native SOL (wrapped)
    "So11111111111111111111111111111111111111112",
    // USDC
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
    // USDT
    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
    // mSOL
    "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So",
    // stSOL
    "7dHbWXmci3dT8UFYWYZweBLXgycu7Y3iL6trKn1Y7ARj",
    // BONK
    "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263",
    // JTO
    "jtojtomepa8beP8AuQc6eXt5FriJwfFMwQx2v2f9mCL",
    // JUP
    "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",
    // PYTH
    "HZ1JovNiVvGrGNiiYvEozEVgZ58xaU3RKwX8eACQBCt3",
    // RAY
    "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R",
    // ORCA
    "orcaEKTdK7LKz57vaAYr9QeNsVEPfiu6QeMU1kektZE",
    // WETH (Wormhole)
    "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs",
    // WBTC (Wormhole)
    "3NZ9JMVBmGAqocybic2c7LQCJScmgsAZ6vQqTDzcqmJh",
    // TRUMP
    "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN",
    // jitoSOL
    "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn",
    // bSOL
    "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1",
    // jupSOL
    "jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v",
    // POPCAT
    "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr",
    // cbBTC (Coinbase wrapped BTC)
    "cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij",
    // WLFI (World Liberty Financial)
    "WLFinEv6ypjkczcS83FZqFpgFZYwQXutRbxGe7oC16g",
    // PYUSD (PayPal USD)
    "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo",
    // USD1 (stablecoin)
    "USD1ttGY1N17NEEHLmELoaybftRBUSErhqYiQzvEmuB",
    // PUMP (pump.fun token)
    "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn",
    // YZY (Yeezy)
    "DrZ26cKJDksVRWib3DVVsjo9eeXccc7hKhDJviiYEEZY",
];

use std::collections::HashSet;
use std::sync::LazyLock;

/// Pre-computed HashSet of supported mint strings for O(1) lookup
pub static SUPPORTED_MINTS_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| SUPPORTED_MINT_STRS.iter().copied().collect());

/// Check if a swap involves only supported mints (tokens we have ATAs for)
#[inline]
pub fn is_supported_swap(buy_mint: &str, sell_mint: &str) -> bool {
    SUPPORTED_MINTS_SET.contains(buy_mint) && SUPPORTED_MINTS_SET.contains(sell_mint)
}

/// Maps a token mint address to its human-readable symbol.
/// Returns abbreviated address if mint is unknown.
pub fn mint_to_symbol(mint: &str) -> &'static str {
    match mint {
        "So11111111111111111111111111111111111111112" => "SOL",
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" => "USDC",
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB" => "USDT",
        "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So" => "mSOL",
        "7dHbWXmci3dT8UFYWYZweBLXgycu7Y3iL6trKn1Y7ARj" => "stSOL",
        "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263" => "BONK",
        "jtojtomepa8beP8AuQc6eXt5FriJwfFMwQx2v2f9mCL" => "JTO",
        "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN" => "JUP",
        "HZ1JovNiVvGrGNiiYvEozEVgZ58xaU3RKwX8eACQBCt3" => "PYTH",
        "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R" => "RAY",
        "orcaEKTdK7LKz57vaAYr9QeNsVEPfiu6QeMU1kektZE" => "ORCA",
        "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs" => "WETH",
        "3NZ9JMVBmGAqocybic2c7LQCJScmgsAZ6vQqTDzcqmJh" => "WBTC",
        "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN" => "TRUMP",
        "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn" => "jitoSOL",
        "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1" => "bSOL",
        "jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v" => "jupSOL",
        "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr" => "POPCAT",
        "cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij" => "cbBTC",
        "WLFinEv6ypjkczcS83FZqFpgFZYwQXutRbxGe7oC16g" => "WLFI",
        "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo" => "PYUSD",
        "USD1ttGY1N17NEEHLmELoaybftRBUSErhqYiQzvEmuB" => "USD1",
        "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn" => "PUMP",
        "DrZ26cKJDksVRWib3DVVsjo9eeXccc7hKhDJviiYEEZY" => "YZY",
        _ => "???",
    }
}
