//! Test Raydium CLMM swap instruction
//!
//! Performs a live swap on mainnet to verify the instruction works.
//! Usage: cargo run --release --bin test_raydium_clmm
//!
//! This will:
//! 1. Derive all possible tick array PDAs for the pool
//! 2. Check which ones exist on-chain via getMultipleAccounts
//! 3. Swap a small amount of SOL → JUP on Raydium CLMM
//! 4. Swap the JUP back to SOL
//! 5. Report success/failure

use anyhow::{anyhow, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::str::FromStr;
use std::time::Duration;

// =============================================================================
// Constants
// =============================================================================

/// Raydium CLMM program
const RAYDIUM_CLMM: Pubkey = solana_sdk::pubkey!("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");

/// Token program
const TOKEN_PROGRAM: Pubkey = solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// ATA program  
const ATA_PROGRAM: Pubkey = solana_sdk::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
/// System program
const SYSTEM_PROGRAM: Pubkey = solana_sdk::pubkey!("11111111111111111111111111111111");
/// WSOL mint
const WSOL_MINT: Pubkey = solana_sdk::pubkey!("So11111111111111111111111111111111111111112");
/// BONK mint
const BONK_MINT: Pubkey = solana_sdk::pubkey!("DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263");

/// SOL/BONK Raydium CLMM pool
/// Pool: GtKKKs3yaPdHbQd2aZS4SfWhy8zQ988BJGnKNndLxYsN
/// token_mint_a: SOL (mint_0)
/// token_mint_b: BONK (mint_1)
const SOL_BONK_CLMM_POOL: Pubkey = solana_sdk::pubkey!("GtKKKs3yaPdHbQd2aZS4SfWhy8zQ988BJGnKNndLxYsN");
const SOL_BONK_CLMM_VAULT_A: Pubkey = solana_sdk::pubkey!("GDnBvA76ZAJ2K3en2F1iExPZ6qz83Xjj5srXMmSKTYDW"); // SOL vault (token_0)
const SOL_BONK_CLMM_VAULT_B: Pubkey = solana_sdk::pubkey!("2wzaFLYb4JcDrVs8TfU3TfkVwq1Pdp3rRgWdNJzFGXud"); // BONK vault (token_1)

/// Pool layout offsets (from on-chain analysis)
const POOL_AMM_CONFIG_OFFSET: usize = 9;
const POOL_OBSERVATION_KEY_OFFSET: usize = 201;
const POOL_TICK_SPACING_OFFSET: usize = 235;
const POOL_TICK_CURRENT_OFFSET: usize = 269;

/// Tick array constants
const TICK_ARRAY_SIZE: i32 = 60;
const MIN_TICK: i32 = -443636;
const MAX_TICK: i32 = 443636;

/// Swap discriminator (Anchor sighash for "global:swap")
const SWAP_DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

// Amount to swap: 0.001 SOL
const SWAP_AMOUNT_LAMPORTS: u64 = 1_000_000;

// =============================================================================
// Tick Array Discovery
// =============================================================================

/// Represents a tick array that exists on-chain
#[derive(Debug, Clone)]
struct TickArrayInfo {
    pubkey: Pubkey,
    start_tick_index: i32,
}

/// Derive tick array PDA for a given start_tick_index
fn derive_tick_array_pda(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    let start_bytes = start_tick_index.to_be_bytes();
    let seeds: &[&[u8]] = &[b"tick_array", pool.as_ref(), &start_bytes];
    Pubkey::find_program_address(seeds, &RAYDIUM_CLMM).0
}

/// Discover which tick arrays exist for a pool by checking all possible PDAs
fn discover_tick_arrays(rpc: &RpcClient, pool: &Pubkey, tick_spacing: u16) -> Result<Vec<TickArrayInfo>> {
    let ticks_per_array = TICK_ARRAY_SIZE * tick_spacing as i32;
    
    // Generate all possible start_tick_index values
    let mut possible_starts: Vec<i32> = Vec::new();
    let mut start = (MIN_TICK / ticks_per_array) * ticks_per_array;
    while start <= MAX_TICK {
        possible_starts.push(start);
        start += ticks_per_array;
    }
    
    println!("Checking {} possible tick array PDAs...", possible_starts.len());
    
    // Derive all PDAs
    let pdas: Vec<(Pubkey, i32)> = possible_starts
        .iter()
        .map(|&start_idx| (derive_tick_array_pda(pool, start_idx), start_idx))
        .collect();
    
    // Check in batches of 100 (RPC limit)
    let mut existing_tick_arrays: Vec<TickArrayInfo> = Vec::new();
    
    for chunk in pdas.chunks(100) {
        let pubkeys: Vec<Pubkey> = chunk.iter().map(|(pk, _)| *pk).collect();
        let accounts = rpc.get_multiple_accounts(&pubkeys)?;
        
        for (i, maybe_account) in accounts.iter().enumerate() {
            if maybe_account.is_some() {
                existing_tick_arrays.push(TickArrayInfo {
                    pubkey: chunk[i].0,
                    start_tick_index: chunk[i].1,
                });
            }
        }
    }
    
    // Sort by start_tick_index
    existing_tick_arrays.sort_by_key(|t| t.start_tick_index);
    
    println!("Found {} existing tick arrays", existing_tick_arrays.len());
    
    Ok(existing_tick_arrays)
}

/// Select tick arrays for a swap based on current tick and direction
/// 
/// IMPORTANT: The first tick array must be the one the contract expects based on 
/// `get_first_initialized_tick_array()`. For zero_for_one, if the current tick's
/// array has no initialized ticks, the contract looks for the first initialized
/// array in that direction (going lower).
/// 
/// Since we don't know which arrays have initialized ticks without parsing their data,
/// we pass ALL arrays in the swap direction starting from the lowest one that exists.
/// The contract will iterate through them to find the right starting point.
fn select_tick_arrays_for_swap(
    all_tick_arrays: &[TickArrayInfo],
    current_tick: i32,
    tick_spacing: u16,
    zero_for_one: bool,
) -> Vec<Pubkey> {
    let ticks_per_array = TICK_ARRAY_SIZE * tick_spacing as i32;
    let current_array_start = (current_tick / ticks_per_array) * ticks_per_array;
    
    // CRITICAL: The FIRST tick array must always be the one containing current tick!
    // Subsequent arrays go in the direction of the swap.
    
    if zero_for_one {
        // For zero_for_one, tick goes DOWN
        // First array contains current tick, then arrays going DOWN (descending order)
        let mut candidates: Vec<&TickArrayInfo> = all_tick_arrays.iter()
            .filter(|ta| ta.start_tick_index <= current_array_start)
            .collect();
        // Sort DESCENDING - highest (containing current) first, then going down
        candidates.sort_by(|a, b| b.start_tick_index.cmp(&a.start_tick_index));
        candidates.into_iter().map(|t| t.pubkey).take(5).collect()
    } else {
        // For one_for_zero, tick goes UP
        // First array contains current tick, then arrays going UP (ascending order)
        let mut candidates: Vec<&TickArrayInfo> = all_tick_arrays.iter()
            .filter(|ta| ta.start_tick_index >= current_array_start)
            .collect();
        // Sort ASCENDING - lowest (containing current) first, then going up
        candidates.sort_by(|a, b| a.start_tick_index.cmp(&b.start_tick_index));
        candidates.into_iter().map(|t| t.pubkey).take(5).collect()
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM.as_ref(), mint.as_ref()],
        &ATA_PROGRAM,
    ).0
}

fn create_ata_idempotent(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: ATA_PROGRAM,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(derive_ata(owner, mint), false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ],
        data: vec![1], // CreateIdempotent
    }
}

fn wrap_sol(payer: &Pubkey, amount: u64) -> Vec<Instruction> {
    let wsol_ata = derive_ata(payer, &WSOL_MINT);
    vec![
        solana_sdk::system_instruction::transfer(payer, &wsol_ata, amount),
        Instruction {
            program_id: TOKEN_PROGRAM,
            accounts: vec![AccountMeta::new(wsol_ata, false)],
            data: vec![17], // SyncNative
        },
    ]
}

fn close_wsol_account(owner: &Pubkey) -> Instruction {
    let wsol_ata = derive_ata(owner, &WSOL_MINT);
    Instruction {
        program_id: TOKEN_PROGRAM,
        accounts: vec![
            AccountMeta::new(wsol_ata, false),
            AccountMeta::new(*owner, false),
            AccountMeta::new_readonly(*owner, true),
        ],
        data: vec![9], // CloseAccount
    }
}

/// Build Raydium CLMM swap instruction
/// 
/// Account order (from successful tx analysis):
/// 0: payer (signer)
/// 1: amm_config
/// 2: pool_state
/// 3: input_token_account (user)
/// 4: output_token_account (user)
/// 5: input_vault
/// 6: output_vault
/// 7: observation_state
/// 8: token_program
/// 9+: tick_arrays (remaining accounts)
fn raydium_clmm_swap_ix(
    pool: Pubkey,
    amm_config: Pubkey,
    observation_state: Pubkey,
    input_vault: Pubkey,
    output_vault: Pubkey,
    user_input: Pubkey,
    user_output: Pubkey,
    tick_arrays: &[Pubkey],
    user: &Pubkey,
    amount: u64,
    other_amount_threshold: u64,
    sqrt_price_limit_x64: u128,
    is_base_input: bool,
    zero_for_one: bool,
) -> Instruction {
    // Per IDL: tick_array is a DEDICATED account at position 9, not a remaining account
    // Additional tick arrays go in remaining_accounts after that
    let first_tick_array = tick_arrays.first().copied().unwrap_or_default();
    
    let mut accounts = vec![
        AccountMeta::new_readonly(*user, true),       // 0: payer
        AccountMeta::new_readonly(amm_config, false), // 1: amm_config
        AccountMeta::new(pool, false),                // 2: pool_state
        AccountMeta::new(user_input, false),          // 3: input_token_account
        AccountMeta::new(user_output, false),         // 4: output_token_account
        AccountMeta::new(input_vault, false),         // 5: input_vault
        AccountMeta::new(output_vault, false),        // 6: output_vault
        AccountMeta::new(observation_state, false),   // 7: observation_state
        AccountMeta::new_readonly(TOKEN_PROGRAM, false), // 8: token_program
        AccountMeta::new(first_tick_array, false),    // 9: tick_array (DEDICATED - must contain current tick)
    ];
    
    // Add remaining tick arrays (skip first since it's dedicated)
    for ta in tick_arrays.iter().skip(1) {
        accounts.push(AccountMeta::new(*ta, false));
    }
    
    // Build instruction data
    let mut data = Vec::with_capacity(8 + 8 + 8 + 16 + 1 + 1);
    data.extend_from_slice(&SWAP_DISCRIMINATOR);
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(&other_amount_threshold.to_le_bytes());
    data.extend_from_slice(&sqrt_price_limit_x64.to_le_bytes());
    data.push(is_base_input as u8);
    data.push(zero_for_one as u8);
    
    Instruction {
        program_id: RAYDIUM_CLMM,
        accounts,
        data,
    }
}

// =============================================================================
// Main
// =============================================================================

fn main() -> Result<()> {
    println!("=== Raydium CLMM Swap Test ===\n");

    // Load config
    let config_str = std::fs::read_to_string("config.json")?;
    let config: serde_json::Value = serde_json::from_str(&config_str)?;
    
    let rpc_url = config["helius_rpc"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing helius_rpc in config"))?;

    if rpc_url.contains("YOUR_API_KEY") {
        return Err(anyhow!("Please set your Helius API key in config.json"));
    }

    // Load payer keypair
    let payer_path = config["payer_keypair"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing payer_keypair in config"))?;
    
    let payer_bytes: Vec<u8> = serde_json::from_str(
        &std::fs::read_to_string(payer_path)?
    )?;
    let payer = Keypair::from_bytes(&payer_bytes)?;

    println!("RPC: {}", rpc_url);
    println!("Payer: {}", payer.pubkey());
    println!("Pool: {} (SOL/BONK CLMM)", SOL_BONK_CLMM_POOL);
    println!("Swap amount: {} lamports (0.001 SOL)\n", SWAP_AMOUNT_LAMPORTS);

    let rpc = RpcClient::new_with_timeout_and_commitment(
        rpc_url.to_string(),
        Duration::from_secs(30),
        CommitmentConfig::confirmed(),
    );

    // Check balance
    let balance = rpc.get_balance(&payer.pubkey())?;
    println!("Current SOL balance: {} lamports ({:.4} SOL)", balance, balance as f64 / 1e9);

    if balance < SWAP_AMOUNT_LAMPORTS + 10_000_000 {
        return Err(anyhow!("Insufficient balance"));
    }

    // =========================================================================
    // Fetch pool state to get amm_config, observation_state, tick info
    // =========================================================================
    println!("\n--- Fetching pool state ---");
    
    let pool_account = rpc.get_account(&SOL_BONK_CLMM_POOL)?;
    let pool_data = &pool_account.data;
    
    // Parse pool state
    let amm_config = Pubkey::try_from(&pool_data[POOL_AMM_CONFIG_OFFSET..POOL_AMM_CONFIG_OFFSET+32])?;
    let observation_state = Pubkey::try_from(&pool_data[POOL_OBSERVATION_KEY_OFFSET..POOL_OBSERVATION_KEY_OFFSET+32])?;
    let tick_spacing = u16::from_le_bytes([pool_data[POOL_TICK_SPACING_OFFSET], pool_data[POOL_TICK_SPACING_OFFSET+1]]);
    let tick_current = i32::from_le_bytes([
        pool_data[POOL_TICK_CURRENT_OFFSET],
        pool_data[POOL_TICK_CURRENT_OFFSET+1],
        pool_data[POOL_TICK_CURRENT_OFFSET+2],
        pool_data[POOL_TICK_CURRENT_OFFSET+3],
    ]);
    
    println!("amm_config: {}", amm_config);
    println!("observation_state: {}", observation_state);
    println!("tick_spacing: {}", tick_spacing);
    println!("tick_current: {}", tick_current);

    // =========================================================================
    // Discover existing tick arrays
    // =========================================================================
    println!("\n--- Discovering tick arrays ---");
    
    let all_tick_arrays = discover_tick_arrays(&rpc, &SOL_BONK_CLMM_POOL, tick_spacing)?;
    
    for ta in &all_tick_arrays {
        let current_marker = if ta.start_tick_index <= tick_current && 
            tick_current < ta.start_tick_index + TICK_ARRAY_SIZE * tick_spacing as i32 {
            " ← current tick here"
        } else {
            ""
        };
        println!("  start={:6} → {}{}", ta.start_tick_index, ta.pubkey, current_marker);
    }

    // Derive ATAs
    let wsol_ata = derive_ata(&payer.pubkey(), &WSOL_MINT);
    let bonk_ata = derive_ata(&payer.pubkey(), &BONK_MINT);

    println!("\nwSOL ATA: {}", wsol_ata);
    println!("BONK ATA: {}", bonk_ata);

    // Check initial BONK balance
    let initial_bonk_balance = match rpc.get_token_account_balance(&bonk_ata) {
        Ok(b) => b.amount.parse::<u64>().unwrap_or(0),
        Err(_) => 0,
    };
    println!("Initial BONK balance: {}\n", initial_bonk_balance);

    // =========================================================================
    // SWAP 1: SOL → BONK
    // token_0 = SOL, token_1 = BONK
    // Selling SOL (token_0) for BONK (token_1)
    // zero_for_one = false (tick goes UP)
    // =========================================================================
    println!("--- Swap 1: SOL → BONK ---");
    
    // Try with zero_for_one = false (price going UP in terms of tick)
    let tick_arrays_swap1 = select_tick_arrays_for_swap(&all_tick_arrays, tick_current, tick_spacing, false);
    println!("Using {} tick arrays for swap", tick_arrays_swap1.len());
    for ta in &tick_arrays_swap1 {
        println!("  {}", ta);
    }

    let mut instructions = vec![];

    // Compute budget
    instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(300_000));
    instructions.push(ComputeBudgetInstruction::set_compute_unit_price(100_000));

    // Create ATAs
    instructions.push(create_ata_idempotent(&payer.pubkey(), &payer.pubkey(), &WSOL_MINT));
    instructions.push(create_ata_idempotent(&payer.pubkey(), &payer.pubkey(), &BONK_MINT));

    // Wrap SOL
    instructions.extend(wrap_sol(&payer.pubkey(), SWAP_AMOUNT_LAMPORTS));

    // Swap SOL → BONK
    // zero_for_one = false (tick goes UP)
    let sqrt_price_limit_x64: u128 = 0;
    
    instructions.push(raydium_clmm_swap_ix(
        SOL_BONK_CLMM_POOL,
        amm_config,
        observation_state,
        SOL_BONK_CLMM_VAULT_A, // input_vault = SOL vault (token_0)
        SOL_BONK_CLMM_VAULT_B, // output_vault = BONK vault (token_1)
        wsol_ata,              // user_input
        bonk_ata,              // user_output
        &tick_arrays_swap1,
        &payer.pubkey(),
        SWAP_AMOUNT_LAMPORTS,  // amount
        1,                     // min_out
        sqrt_price_limit_x64,
        true,                  // is_base_input
        false,                 // zero_for_one = false (tick goes UP)
    ));

    // Close wSOL
    instructions.push(close_wsol_account(&payer.pubkey()));

    let blockhash = rpc.get_latest_blockhash()?;
    let message = Message::new(&instructions, Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(message);
    tx.sign(&[&payer], blockhash);

    println!("\nSending swap 1 transaction...");
    match rpc.send_and_confirm_transaction_with_spinner(&tx) {
        Ok(sig) => {
            println!("✓ Swap 1 SUCCESS: https://solscan.io/tx/{}", sig);
        }
        Err(e) => {
            println!("✗ Swap 1 FAILED: {:?}", e);
            return Err(anyhow!("Swap 1 failed: {:?}", e));
        }
    }

    // Check BONK balance
    std::thread::sleep(Duration::from_secs(2));
    let bonk_balance_after = match rpc.get_token_account_balance(&bonk_ata) {
        Ok(b) => b.amount.parse::<u64>().unwrap_or(0),
        Err(e) => return Err(anyhow!("Failed to get BONK balance: {:?}", e)),
    };
    let bonk_received = bonk_balance_after.saturating_sub(initial_bonk_balance);
    println!("BONK received: {} (raw)\n", bonk_received);

    if bonk_received == 0 {
        return Err(anyhow!("Swap 1 returned 0 BONK"));
    }

    // =========================================================================
    // SWAP 2: BONK → SOL
    // Selling BONK (token_1) for SOL (token_0)
    // zero_for_one = true (tick goes DOWN)
    // =========================================================================
    println!("--- Swap 2: BONK → SOL ---");
    
    // Re-fetch pool state for updated tick
    let pool_account2 = rpc.get_account(&SOL_BONK_CLMM_POOL)?;
    let pool_data2 = &pool_account2.data;
    let tick_current2 = i32::from_le_bytes([
        pool_data2[POOL_TICK_CURRENT_OFFSET],
        pool_data2[POOL_TICK_CURRENT_OFFSET+1],
        pool_data2[POOL_TICK_CURRENT_OFFSET+2],
        pool_data2[POOL_TICK_CURRENT_OFFSET+3],
    ]);
    println!("Current tick after swap 1: {}", tick_current2);
    
    // Select tick arrays for reverse direction (zero_for_one = true = tick going DOWN)
    let tick_arrays_swap2 = select_tick_arrays_for_swap(&all_tick_arrays, tick_current2, tick_spacing, true);
    println!("Using {} tick arrays for swap", tick_arrays_swap2.len());

    let mut instructions2 = vec![];

    instructions2.push(ComputeBudgetInstruction::set_compute_unit_limit(300_000));
    instructions2.push(ComputeBudgetInstruction::set_compute_unit_price(100_000));
    instructions2.push(create_ata_idempotent(&payer.pubkey(), &payer.pubkey(), &WSOL_MINT));

    // Swap BONK → SOL
    // zero_for_one = true (tick goes DOWN)
    let sqrt_price_limit_x64_2: u128 = 0;
    
    instructions2.push(raydium_clmm_swap_ix(
        SOL_BONK_CLMM_POOL,
        amm_config,
        observation_state,
        SOL_BONK_CLMM_VAULT_B, // input_vault = BONK vault (token_1)
        SOL_BONK_CLMM_VAULT_A, // output_vault = SOL vault (token_0)
        bonk_ata,              // user_input
        wsol_ata,              // user_output
        &tick_arrays_swap2,
        &payer.pubkey(),
        bonk_received,         // amount
        1,                     // min_out
        sqrt_price_limit_x64_2,
        true,                  // is_base_input
        true,                  // zero_for_one = true (tick goes DOWN)
    ));

    instructions2.push(close_wsol_account(&payer.pubkey()));

    let blockhash2 = rpc.get_latest_blockhash()?;
    let message2 = Message::new(&instructions2, Some(&payer.pubkey()));
    let mut tx2 = Transaction::new_unsigned(message2);
    tx2.sign(&[&payer], blockhash2);

    println!("\nSending swap 2 transaction...");
    match rpc.send_and_confirm_transaction_with_spinner(&tx2) {
        Ok(sig) => {
            println!("✓ Swap 2 SUCCESS: https://solscan.io/tx/{}", sig);
        }
        Err(e) => {
            println!("✗ Swap 2 FAILED: {:?}", e);
            return Err(anyhow!("Swap 2 failed: {:?}", e));
        }
    }

    // Final summary
    std::thread::sleep(Duration::from_secs(2));
    let final_balance = rpc.get_balance(&payer.pubkey())?;
    let sol_lost = balance.saturating_sub(final_balance);
    
    println!("\n=== Summary ===");
    println!("Initial SOL: {} lamports", balance);
    println!("Final SOL:   {} lamports", final_balance);
    println!("Net loss:    {} lamports ({:.6} SOL)", sol_lost, sol_lost as f64 / 1e9);
    println!("\n✓ Both CLMM swaps completed successfully!");

    Ok(())
}
