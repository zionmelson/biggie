/// Shared constants used across the arbitrage bot.
pub const WSOL_MINT_STR: &str = "So11111111111111111111111111111111111111112";

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
    // WEN
    "WENWENvqqNya429ubCdR81ZmD69brwQaaBYY6p3LCpk",
    // POPCAT
    "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr",
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
        "WENWENvqqNya429ubCdR81ZmD69brwQaaBYY6p3LCpk" => "WEN",
        "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr" => "POPCAT",
        _ => "???",
    }
}
