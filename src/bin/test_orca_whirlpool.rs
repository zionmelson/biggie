//! Test Orca Whirlpool swap functionality
//!
//! This binary tests the Whirlpool swap instruction directly on mainnet.
//!
//! Usage:
//!   cargo run --release --bin test_orca_whirlpool

use anyhow::{anyhow, Result};
use base64::Engine;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::str::FromStr;

// ============================================================================
// Constants
// ============================================================================

/// Orca Whirlpool program ID
const WHIRLPOOL_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

/// Token program
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Associated token program
const ATA_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

/// Wrapped SOL mint
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Swap instruction discriminator
const SWAP_DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

/// Tick array size (ticks per array)
const TICK_ARRAY_SIZE: i32 = 88;

/// Max/Min sqrt price limits
const MIN_SQRT_PRICE_X64: u128 = 4295048016;
const MAX_SQRT_PRICE_X64: u128 = 79226673515401279992447579055;

// ============================================================================
// Whirlpool State Parsing
// ============================================================================

/// Whirlpool account layout (from source code tests):
/// discriminator: 8 bytes (offset 0)
/// whirlpools_config: 32 bytes (offset 8)
/// whirlpool_bump: 1 byte (offset 40)
/// tick_spacing: 2 bytes (offset 41)
/// fee_tier_index_seed: 2 bytes (offset 43)
/// fee_rate: 2 bytes (offset 45)
/// protocol_fee_rate: 2 bytes (offset 47)
/// liquidity: 16 bytes (offset 49)
/// sqrt_price: 16 bytes (offset 65)
/// tick_current_index: 4 bytes (offset 81)
/// protocol_fee_owed_a: 8 bytes (offset 85)
/// protocol_fee_owed_b: 8 bytes (offset 93)
/// token_mint_a: 32 bytes (offset 101)
/// token_vault_a: 32 bytes (offset 133)
/// fee_growth_global_a: 16 bytes (offset 165)
/// token_mint_b: 32 bytes (offset 181)
/// token_vault_b: 32 bytes (offset 213)
#[derive(Debug)]
struct WhirlpoolState {
    tick_spacing: u16,
    sqrt_price: u128,
    tick_current_index: i32,
    token_mint_a: Pubkey,
    token_mint_b: Pubkey,
    token_vault_a: Pubkey,
    token_vault_b: Pubkey,
}

fn parse_whirlpool(data: &[u8]) -> Result<WhirlpoolState> {
    if data.len() < 245 {
        return Err(anyhow!("Whirlpool data too short: {} bytes", data.len()));
    }
    
    // Verify discriminator
    let discriminator = &data[0..8];
    let expected: [u8; 8] = [0x3f, 0x95, 0xd1, 0x0c, 0xe1, 0x80, 0x63, 0x09];
    if discriminator != expected {
        return Err(anyhow!("Invalid whirlpool discriminator: {:?}", discriminator));
    }
    
    let tick_spacing = u16::from_le_bytes([data[41], data[42]]);
    
    let sqrt_price = u128::from_le_bytes(data[65..81].try_into()?);
    
    let tick_current_index = i32::from_le_bytes([data[81], data[82], data[83], data[84]]);
    
    let token_mint_a = Pubkey::new_from_array(data[101..133].try_into()?);
    let token_vault_a = Pubkey::new_from_array(data[133..165].try_into()?);
    // Note: fee_growth_global_a is 16 bytes at 165-181
    let token_mint_b = Pubkey::new_from_array(data[181..213].try_into()?);
    let token_vault_b = Pubkey::new_from_array(data[213..245].try_into()?);
    
    Ok(WhirlpoolState {
        tick_spacing,
        sqrt_price,
        tick_current_index,
        token_mint_a,
        token_mint_b,
        token_vault_a,
        token_vault_b,
    })
}

// ============================================================================
// PDA Derivations
// ============================================================================

fn whirlpool_program() -> Pubkey {
    Pubkey::from_str(WHIRLPOOL_PROGRAM).unwrap()
}

fn token_program() -> Pubkey {
    Pubkey::from_str(TOKEN_PROGRAM).unwrap()
}

fn ata_program() -> Pubkey {
    Pubkey::from_str(ATA_PROGRAM).unwrap()
}

fn wsol_mint() -> Pubkey {
    Pubkey::from_str(WSOL_MINT).unwrap()
}

/// Derive oracle PDA for a whirlpool
fn derive_oracle_pda(whirlpool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"oracle", whirlpool.as_ref()],
        &whirlpool_program(),
    ).0
}

/// Derive tick array PDA
/// NOTE: Orca uses the STRING representation of the tick index as the seed!
fn derive_tick_array_pda(whirlpool: &Pubkey, start_tick_index: i32) -> Pubkey {
    let tick_str = start_tick_index.to_string();
    Pubkey::find_program_address(
        &[b"tick_array", whirlpool.as_ref(), tick_str.as_bytes()],
        &whirlpool_program(),
    ).0
}

/// Get associated token account
fn get_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program().as_ref(), mint.as_ref()],
        &ata_program(),
    ).0
}

/// Calculate tick array start index for a given tick
fn tick_array_start_index(tick_index: i32, tick_spacing: u16) -> i32 {
    let ticks_per_array = TICK_ARRAY_SIZE * (tick_spacing as i32);
    let mut start = (tick_index / ticks_per_array) * ticks_per_array;
    if tick_index < 0 && tick_index % ticks_per_array != 0 {
        start -= ticks_per_array;
    }
    start
}

/// Find tick arrays for swap
/// Returns 3 tick arrays in the direction of the swap
fn find_tick_arrays_for_swap(
    whirlpool: &Pubkey,
    current_tick: i32,
    tick_spacing: u16,
    a_to_b: bool,
) -> Vec<Pubkey> {
    let ticks_per_array = TICK_ARRAY_SIZE * (tick_spacing as i32);
    let current_start = tick_array_start_index(current_tick, tick_spacing);
    
    let mut arrays = Vec::with_capacity(3);
    
    if a_to_b {
        // A→B: tick goes DOWN, need current and lower arrays
        for i in 0..3 {
            let start = current_start - (i * ticks_per_array);
            arrays.push(derive_tick_array_pda(whirlpool, start));
        }
    } else {
        // B→A: tick goes UP, need current and higher arrays
        for i in 0..3 {
            let start = current_start + (i * ticks_per_array);
            arrays.push(derive_tick_array_pda(whirlpool, start));
        }
    }
    
    arrays
}

// ============================================================================
// Swap Instruction Builder
// ============================================================================

fn whirlpool_swap_ix(
    whirlpool_addr: &Pubkey,
    state: &WhirlpoolState,
    payer: &Pubkey,
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
) -> Result<Instruction> {
    // Derive oracle
    let oracle = derive_oracle_pda(whirlpool_addr);
    
    // Find tick arrays
    let tick_arrays = find_tick_arrays_for_swap(
        whirlpool_addr,
        state.tick_current_index,
        state.tick_spacing,
        a_to_b,
    );
    
    if tick_arrays.len() < 3 {
        return Err(anyhow!("Need 3 tick arrays, got {}", tick_arrays.len()));
    }
    
    // User token accounts
    let user_ata_a = get_ata(payer, &state.token_mint_a);
    let user_ata_b = get_ata(payer, &state.token_mint_b);
    
    // sqrt_price_limit based on direction
    let sqrt_price_limit: u128 = if a_to_b {
        MIN_SQRT_PRICE_X64 + 1 // Going down, use min limit
    } else {
        MAX_SQRT_PRICE_X64 - 1 // Going up, use max limit
    };
    
    // Account layout:
    // 0:  token_program
    // 1:  token_authority (signer)
    // 2:  whirlpool (writable)
    // 3:  token_owner_account_a (writable)
    // 4:  token_vault_a (writable)
    // 5:  token_owner_account_b (writable)
    // 6:  token_vault_b (writable)
    // 7:  tick_array_0 (writable)
    // 8:  tick_array_1 (writable)
    // 9:  tick_array_2 (writable)
    // 10: oracle
    
    let accounts = vec![
        AccountMeta::new_readonly(token_program(), false),
        AccountMeta::new_readonly(*payer, true),
        AccountMeta::new(*whirlpool_addr, false),
        AccountMeta::new(user_ata_a, false),
        AccountMeta::new(state.token_vault_a, false),
        AccountMeta::new(user_ata_b, false),
        AccountMeta::new(state.token_vault_b, false),
        AccountMeta::new(tick_arrays[0], false),
        AccountMeta::new(tick_arrays[1], false),
        AccountMeta::new(tick_arrays[2], false),
        AccountMeta::new(oracle, false), // Oracle needs to be writable for AdaptiveFee pools
    ];
    
    // Instruction data:
    // - discriminator: 8 bytes
    // - amount: u64 (8 bytes)
    // - other_amount_threshold: u64 (8 bytes)
    // - sqrt_price_limit: u128 (16 bytes)
    // - amount_specified_is_input: bool (1 byte)
    // - a_to_b: bool (1 byte)
    
    let mut data = Vec::with_capacity(42);
    data.extend_from_slice(&SWAP_DISCRIMINATOR);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());
    data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
    data.push(1u8); // amount_specified_is_input = true
    data.push(if a_to_b { 1u8 } else { 0u8 }); // a_to_b
    
    Ok(Instruction {
        program_id: whirlpool_program(),
        accounts,
        data,
    })
}

// ============================================================================
// wSOL Helpers
// ============================================================================

/// Create ATA instruction (idempotent)
fn create_ata_ix(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: ata_program(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(get_ata(owner, mint), false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(Pubkey::from_str("11111111111111111111111111111111").unwrap(), false),
            AccountMeta::new_readonly(token_program(), false),
        ],
        data: vec![1], // CreateIdempotent
    }
}

/// Wrap SOL: transfer SOL to wSOL ATA then sync
fn wrap_sol_ix(payer: &Pubkey, amount: u64) -> Vec<Instruction> {
    let wsol_ata = get_ata(payer, &wsol_mint());
    vec![
        solana_sdk::system_instruction::transfer(payer, &wsol_ata, amount),
        Instruction {
            program_id: token_program(),
            accounts: vec![AccountMeta::new(wsol_ata, false)],
            data: vec![17], // SyncNative
        },
    ]
}

/// Close wSOL account to recover SOL
fn close_wsol_ix(owner: &Pubkey) -> Instruction {
    let wsol_ata = get_ata(owner, &wsol_mint());
    Instruction {
        program_id: token_program(),
        accounts: vec![
            AccountMeta::new(wsol_ata, false),
            AccountMeta::new(*owner, false),
            AccountMeta::new_readonly(*owner, true),
        ],
        data: vec![9], // CloseAccount
    }
}

// ============================================================================
// Main Test
// ============================================================================

fn main() -> Result<()> {
    println!("=== Orca Whirlpool Swap Test ===\n");
    
    // Load wallet
    let wallet_path = std::env::var("WALLET_PATH")
        .unwrap_or_else(|_| format!("{}/.config/solana/id.json", std::env::var("HOME").unwrap()));
    let wallet_bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(&wallet_path)?)?;
    let keypair = Keypair::from_bytes(&wallet_bytes)?;
    let payer = keypair.pubkey();
    println!("Wallet: {}", payer);
    
    // Connect to RPC
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let rpc = RpcClient::new_with_commitment(&rpc_url, CommitmentConfig::confirmed());
    
    // SOL balance
    let balance = rpc.get_balance(&payer)?;
    println!("SOL Balance: {} SOL\n", balance as f64 / 1e9);
    
    // Test pool: SOL/BONK Whirlpool
    let whirlpool_addr = Pubkey::from_str("5r2SKyVDAmaj8HJak4qRS2oLCn2caf3jmrJi8ikRKj6s")?;
    println!("Testing Whirlpool: {} (SOL/BONK)", whirlpool_addr);
    
    // Fetch pool state
    let account = rpc.get_account(&whirlpool_addr)?;
    println!("Pool account owner: {}", account.owner);
    println!("Pool data length: {} bytes", account.data.len());
    
    let state = parse_whirlpool(&account.data)?;
    println!("\nWhirlpool State:");
    println!("  tick_spacing: {}", state.tick_spacing);
    println!("  current_tick: {}", state.tick_current_index);
    println!("  sqrt_price: {}", state.sqrt_price);
    println!("  token_mint_a: {}", state.token_mint_a);
    println!("  token_mint_b: {}", state.token_mint_b);
    println!("  token_vault_a: {}", state.token_vault_a);
    println!("  token_vault_b: {}", state.token_vault_b);
    
    // Derive oracle
    let oracle = derive_oracle_pda(&whirlpool_addr);
    println!("\n  Derived oracle: {}", oracle);
    
    // Find tick arrays
    let tick_arrays = find_tick_arrays_for_swap(
        &whirlpool_addr,
        state.tick_current_index,
        state.tick_spacing,
        true, // a_to_b for SOL→USDC
    );
    println!("\n  Tick arrays for SOL→USDC:");
    for (i, ta) in tick_arrays.iter().enumerate() {
        println!("    {}: {}", i, ta);
    }
    
    // Verify tick arrays exist
    println!("\nVerifying tick arrays exist...");
    for (i, ta) in tick_arrays.iter().enumerate() {
        match rpc.get_account(ta) {
            Ok(acc) => println!("  TickArray {}: {} bytes, owner: {}", i, acc.data.len(), acc.owner),
            Err(e) => println!("  TickArray {}: NOT FOUND - {}", i, e),
        }
    }
    
    // Check token accounts
    let user_ata_a = get_ata(&payer, &state.token_mint_a);
    let user_ata_b = get_ata(&payer, &state.token_mint_b);
    println!("\nToken accounts:");
    println!("  User ATA A (SOL/wSOL): {}", user_ata_a);
    println!("  User ATA B (USDC): {}", user_ata_b);
    
    // Check balances
    match rpc.get_token_account_balance(&user_ata_a) {
        Ok(bal) => println!("  Token A balance: {}", bal.ui_amount_string),
        Err(_) => println!("  Token A account not found"),
    }
    match rpc.get_token_account_balance(&user_ata_b) {
        Ok(bal) => println!("  Token B balance: {}", bal.ui_amount_string),
        Err(_) => println!("  Token B account not found"),
    }
    
    // ========================================================================
    // Execute swap: SOL → BONK (A → B)
    // ========================================================================
    println!("\n=== Executing SOL → BONK Swap ===");
    
    // Swap 0.001 SOL for BONK (SOL has 9 decimals)
    let amount_in: u64 = 1_000_000; // 0.001 SOL
    let min_amount_out: u64 = 1; // Minimum BONK expected
    let a_to_b = true; // A→B = SOL→BONK
    
    println!("Swap: {} SOL → BONK (min {} BONK)", amount_in as f64 / 1e9, min_amount_out);
    
    // Build instructions
    let mut instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(400_000),
        ComputeBudgetInstruction::set_compute_unit_price(50_000),
    ];
    
    // Create wSOL ATA if needed
    instructions.push(create_ata_ix(&payer, &payer, &wsol_mint()));
    
    // Create BONK ATA if needed
    instructions.push(create_ata_ix(&payer, &payer, &state.token_mint_b));
    
    // Wrap SOL
    instructions.extend(wrap_sol_ix(&payer, amount_in));
    
    // Build swap instruction
    let swap_ix = whirlpool_swap_ix(
        &whirlpool_addr,
        &state,
        &payer,
        amount_in,
        min_amount_out,
        a_to_b,
    )?;
    instructions.push(swap_ix.clone());
    
    // Close wSOL account to recover rent + remaining SOL
    instructions.push(close_wsol_ix(&payer));
    
    println!("Swap instruction accounts: {}", swap_ix.accounts.len());
    for (i, acc) in swap_ix.accounts.iter().enumerate() {
        println!("  {}: {} (w={}, s={})", i, acc.pubkey, acc.is_writable, acc.is_signer);
    }
    println!("Instruction data: {} bytes", swap_ix.data.len());
    println!("  Discriminator: {:?}", &swap_ix.data[0..8]);
    
    // Build transaction
    let recent_blockhash = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer),
        &[&keypair],
        recent_blockhash,
    );
    
    // Simulate first
    println!("\nSimulating transaction...");
    match rpc.simulate_transaction(&tx) {
        Ok(result) => {
            if let Some(err) = result.value.err {
                println!("Simulation failed: {:?}", err);
                if let Some(logs) = result.value.logs {
                    println!("Logs:");
                    for log in logs {
                        println!("  {}", log);
                    }
                }
                return Err(anyhow!("Simulation failed"));
            }
            println!("Simulation succeeded!");
            println!("  Units consumed: {:?}", result.value.units_consumed);
        }
        Err(e) => {
            println!("Simulation error: {}", e);
            return Err(anyhow!("Simulation error: {}", e));
        }
    }
    
    // Send transaction
    println!("\nSending transaction...");
    match rpc.send_and_confirm_transaction(&tx) {
        Ok(sig) => {
            println!("SUCCESS! Signature: {}", sig);
            println!("https://solscan.io/tx/{}", sig);
        }
        Err(e) => {
            println!("Transaction failed: {}", e);
            return Err(anyhow!("Transaction failed: {}", e));
        }
    }
    
    println!("\n=== Orca Whirlpool swap test complete ===");
    Ok(())
}
