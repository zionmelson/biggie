//! Test PancakeSwap CLMM swap functionality
//!
//! Usage:
//!   cargo run --release --bin test_pancakeswap_clmm

use anyhow::{anyhow, Result};
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

const PANCAKESWAP_CLMM_PROGRAM: &str = "HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// PancakeSwap uses swap_v2 instruction, not swap
/// Discriminator = sha256("global:swap_v2")[0..8]
const SWAP_V2_DISCRIMINATOR: [u8; 8] = [0x2b, 0x04, 0xed, 0x0b, 0x1a, 0xc9, 0x1e, 0x62];

const TICK_ARRAY_SIZE: i32 = 60; // PancakeSwap uses 60 ticks per array

// ============================================================================
// Pool State Parsing
// ============================================================================

#[derive(Debug)]
struct PancakePoolState {
    amm_config: Pubkey,
    token_mint_0: Pubkey,
    token_mint_1: Pubkey,
    token_vault_0: Pubkey,
    token_vault_1: Pubkey,
    observation_key: Pubkey,
    tick_spacing: u16,
    tick_current: i32,
}

fn parse_pool_state(data: &[u8]) -> Result<PancakePoolState> {
    if data.len() < 280 {
        return Err(anyhow!("Pool data too short: {} bytes", data.len()));
    }
    
    // PancakeSwap CLMM layout:
    // discriminator: 8 bytes (offset 0)
    // bump: 1 byte (offset 8)
    // amm_config: 32 bytes (offset 9)
    // owner: 32 bytes (offset 41)
    // token_mint_0: 32 bytes (offset 73)
    // token_mint_1: 32 bytes (offset 105)
    // token_vault_0: 32 bytes (offset 137)
    // token_vault_1: 32 bytes (offset 169)
    // observation_key: 32 bytes (offset 201)
    // mint_decimals_0: 1 byte (offset 233)
    // mint_decimals_1: 1 byte (offset 234)
    // tick_spacing: 2 bytes (offset 235)
    // liquidity: 16 bytes (offset 237)
    // sqrt_price_x64: 16 bytes (offset 253)
    // tick_current: 4 bytes (offset 269)

    let amm_config = Pubkey::new_from_array(data[9..41].try_into()?);
    let token_mint_0 = Pubkey::new_from_array(data[73..105].try_into()?);
    let token_mint_1 = Pubkey::new_from_array(data[105..137].try_into()?);
    let token_vault_0 = Pubkey::new_from_array(data[137..169].try_into()?);
    let token_vault_1 = Pubkey::new_from_array(data[169..201].try_into()?);
    let observation_key = Pubkey::new_from_array(data[201..233].try_into()?);
    let tick_spacing = u16::from_le_bytes([data[235], data[236]]);
    let tick_current = i32::from_le_bytes([data[269], data[270], data[271], data[272]]);

    Ok(PancakePoolState {
        amm_config,
        token_mint_0,
        token_mint_1,
        token_vault_0,
        token_vault_1,
        observation_key,
        tick_spacing,
        tick_current,
    })
}

// ============================================================================
// PDA Derivations
// ============================================================================

fn pancakeswap_program() -> Pubkey {
    Pubkey::from_str(PANCAKESWAP_CLMM_PROGRAM).unwrap()
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

/// Derive tick array PDA - PancakeSwap uses BIG ENDIAN bytes for tick index
fn derive_tick_array_pda(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[b"tick_array", pool.as_ref(), &start_tick_index.to_be_bytes()],
        &pancakeswap_program(),
    ).0
}

fn get_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program().as_ref(), mint.as_ref()],
        &ata_program(),
    ).0
}

fn tick_array_start_index(tick_index: i32, tick_spacing: u16) -> i32 {
    let ticks_per_array = TICK_ARRAY_SIZE * (tick_spacing as i32);
    let mut start = (tick_index / ticks_per_array) * ticks_per_array;
    if tick_index < 0 && tick_index % ticks_per_array != 0 {
        start -= ticks_per_array;
    }
    start
}

fn find_tick_arrays_for_swap(
    pool: &Pubkey,
    current_tick: i32,
    tick_spacing: u16,
    zero_for_one: bool,
) -> Vec<Pubkey> {
    let ticks_per_array = TICK_ARRAY_SIZE * (tick_spacing as i32);
    let current_start = tick_array_start_index(current_tick, tick_spacing);
    
    let mut arrays = Vec::with_capacity(3);
    
    if zero_for_one {
        // Tick goes DOWN - need current and lower arrays
        for i in 0..3 {
            let start = current_start - (i * ticks_per_array);
            arrays.push(derive_tick_array_pda(pool, start));
        }
    } else {
        // Tick goes UP - need current and higher arrays
        for i in 0..3 {
            let start = current_start + (i * ticks_per_array);
            arrays.push(derive_tick_array_pda(pool, start));
        }
    }
    
    arrays
}

// ============================================================================
// Swap Instruction Builder
// ============================================================================

fn swap_ix_with_bitmap(
    pool_addr: &Pubkey,
    state: &PancakePoolState,
    payer: &Pubkey,
    amount_in: u64,
    min_amount_out: u64,
    zero_for_one: bool,
    bitmap_extension: &Pubkey,
    tick_arrays: &[Pubkey], // Now passed in instead of computed
) -> Result<Instruction> {
    // User token accounts
    let user_ata_0 = get_ata(payer, &state.token_mint_0);
    let user_ata_1 = get_ata(payer, &state.token_mint_1);

    let (input_vault, output_vault, user_input, user_output, input_mint, output_mint) = if zero_for_one {
        // zero_for_one = true: Selling token_0 for token_1 (e.g., BONK→USDC)
        (state.token_vault_0, state.token_vault_1, user_ata_0, user_ata_1, state.token_mint_0, state.token_mint_1)
    } else {
        // zero_for_one = false: Selling token_1 for token_0 (e.g., USDC→BONK)
        (state.token_vault_1, state.token_vault_0, user_ata_1, user_ata_0, state.token_mint_1, state.token_mint_0)
    };
    
    // Programs
    let token_program_2022 = Pubkey::from_str("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb").unwrap();
    let memo_program = Pubkey::from_str("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr").unwrap();

    // PancakeSwap CLMM swap_v2 account layout (from pancake-frontend SDK):
    // Based on ClmmInstrument.swapInstruction() in instrument.ts
    //
    // Main accounts (13):
    // 0: payer (signer)
    // 1: ammConfigId
    // 2: poolId 
    // 3: inputTokenAccount (user)
    // 4: outputTokenAccount (user)
    // 5: inputVault
    // 6: outputVault
    // 7: observationId
    // 8: TOKEN_PROGRAM_ID
    // 9: TOKEN_2022_PROGRAM_ID
    // 10: MEMO_PROGRAM_ID
    // 11: inputMint
    // 12: outputMint
    //
    // Then remainingAccounts:
    // 13: exTickArrayBitmap (FIRST in remaining!)
    // 14-16: tickArrays[0..3]
    let mut accounts = vec![
        AccountMeta::new_readonly(*payer, true),           // 0 - payer
        AccountMeta::new_readonly(state.amm_config, false), // 1 - ammConfigId
        AccountMeta::new(*pool_addr, false),                // 2 - poolId
        AccountMeta::new(user_input, false),                // 3 - inputTokenAccount
        AccountMeta::new(user_output, false),               // 4 - outputTokenAccount
        AccountMeta::new(input_vault, false),               // 5 - inputVault
        AccountMeta::new(output_vault, false),              // 6 - outputVault
        AccountMeta::new(state.observation_key, false),     // 7 - observationId
        AccountMeta::new_readonly(token_program(), false),  // 8 - TOKEN_PROGRAM_ID
        AccountMeta::new_readonly(token_program_2022, false), // 9 - TOKEN_2022_PROGRAM_ID
        AccountMeta::new_readonly(memo_program, false),     // 10 - MEMO_PROGRAM_ID
        AccountMeta::new_readonly(input_mint, false),       // 11 - inputMint
        AccountMeta::new_readonly(output_mint, false),      // 12 - outputMint
    ];
    
    // Remaining accounts: bitmap extension FIRST, then tick arrays
    accounts.push(AccountMeta::new(*bitmap_extension, false));
    
    for ta in tick_arrays {
        accounts.push(AccountMeta::new(*ta, false));
    }

    // swap_v2 instruction data format (41 bytes):
    // - discriminator: 8 bytes
    // - amount: u64 (8 bytes)
    // - other_amount_threshold: u64 (8 bytes)
    // - sqrt_price_limit_x64: u128 (16 bytes) - use 0 for no limit
    // - is_base_input: bool (1 byte) - true = exact input swap
    let sqrt_price_limit: u128 = 0; // Use 0 for no sqrt price limit
    
    let mut data = Vec::with_capacity(41);
    data.extend_from_slice(&SWAP_V2_DISCRIMINATOR);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());
    data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
    // is_base_input = true (1) for exact input swaps
    data.push(1u8);

    Ok(Instruction {
        program_id: pancakeswap_program(),
        accounts,
        data,
    })
}

// ============================================================================
// wSOL Helpers
// ============================================================================

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
    println!("=== PancakeSwap CLMM Swap Test ===\n");
    
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
    
    let balance = rpc.get_balance(&payer)?;
    println!("SOL Balance: {} SOL\n", balance as f64 / 1e9);
    
    // Test pool: BONK/USDC on PancakeSwap (has active tick arrays)
    let pool_addr = Pubkey::from_str("Hgnmt4WVNEkDVhu7yQYFKUf9V7DrGdyPCJSuS4pLAG2U")?;
    println!("Testing Pool: {} (BONK/USDC)", pool_addr);
    
    // Fetch pool state
    let account = rpc.get_account(&pool_addr)?;
    println!("Pool owner: {}", account.owner);
    println!("Pool data: {} bytes", account.data.len());
    
    let state = parse_pool_state(&account.data)?;
    println!("\nPool State:");
    println!("  amm_config: {}", state.amm_config);
    println!("  token_mint_0: {} (BONK)", state.token_mint_0);
    println!("  token_mint_1: {} (USDC)", state.token_mint_1);
    println!("  token_vault_0: {}", state.token_vault_0);
    println!("  token_vault_1: {}", state.token_vault_1);
    println!("  observation_key: {}", state.observation_key);
    println!("  tick_spacing: {}", state.tick_spacing);
    println!("  tick_current: {}", state.tick_current);
    
    // Find tick arrays
    // USDC→BONK = selling token_1 for token_0 = zero_for_one = FALSE
    // When zero_for_one=false, tick goes UP, need higher tick arrays
    let zero_for_one = false;
    let tick_arrays = find_tick_arrays_for_swap(
        &pool_addr,
        state.tick_current,
        state.tick_spacing,
        zero_for_one,
    );
    
    println!("\nTick arrays for swap (zero_for_one={}):", zero_for_one);
    for (i, ta) in tick_arrays.iter().enumerate() {
        match rpc.get_account(ta) {
            Ok(acc) => println!("  {}: {} ({} bytes)", i, ta, acc.data.len()),
            Err(_) => println!("  {}: {} (NOT FOUND)", i, ta),
        }
    }
    
    // Bitmap extension for this pool (found via RPC query)
    let bitmap_extension = Pubkey::from_str("FCxMgS5T2BuqkgRc3Q74e2JkD4ey7u617ADftqkaNxqN")?;
    match rpc.get_account(&bitmap_extension) {
        Ok(acc) => println!("\nBitmap extension: {} ({} bytes)", bitmap_extension, acc.data.len()),
        Err(_) => println!("\nBitmap extension: NOT FOUND"),
    }
    
    // ========================================================================
    // Execute swap: BONK → USDC
    // ========================================================================
    println!("\n=== Executing BONK → USDC Swap ===");
    
    // Swap 1000 BONK for USDC (BONK has 5 decimals)
    let amount_in: u64 = 100_000_000; // 1000 BONK
    let min_amount_out: u64 = 1;
    
    // BONK→USDC = selling token_0 (BONK) for token_1 (USDC) = zero_for_one = TRUE
    let zero_for_one = true;
    
    println!("Swap: {} BONK → USDC (min {} USDC)", amount_in as f64 / 1e5, min_amount_out as f64 / 1e6);
    
    // Check BONK balance
    let bonk_mint = state.token_mint_0;
    let user_bonk = get_ata(&payer, &bonk_mint);
    match rpc.get_token_account_balance(&user_bonk) {
        Ok(bal) => println!("BONK balance: {}", bal.ui_amount_string),
        Err(_) => {
            println!("No BONK balance - cannot test");
            return Err(anyhow!("Need BONK to test this swap"));
        }
    }
    
    // Hardcode tick arrays that we know exist with liquidity for this pool
    // Found via RPC query of getProgramAccounts
    // For BONK→USDC swap (zero_for_one=true, tick goes DOWN from -90426)
    // Need arrays below current tick: -90600, -91200, -91800
    let tick_arrays = vec![
        Pubkey::from_str("EpEYL2d5ZVxg4Wtj3JUD9ExaqFLz3UasTSvjr2jtq99X")?, // start=-90600 (current tick is in this one)
        Pubkey::from_str("72cXxzhfLd4VtzwCwsZUCdXGkjvtQ7Jdbyso6xg3sMxT")?, // start=-91200
        Pubkey::from_str("AsTNWmZ4r4o3schowGVu5mkD1Sft1jssN7rzExRNwaeL")?, // start=-91800
    ];
    
    let mut instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(400_000),
        ComputeBudgetInstruction::set_compute_unit_price(50_000),
    ];
    
    // Create USDC ATA if needed  
    instructions.push(create_ata_ix(&payer, &payer, &state.token_mint_1));
    
    // Build swap instruction with bitmap extension
    let swap_ix_data = swap_ix_with_bitmap(
        &pool_addr,
        &state,
        &payer,
        amount_in,
        min_amount_out,
        zero_for_one,
        &bitmap_extension,
        &tick_arrays,
    )?;
    instructions.push(swap_ix_data.clone());
    
    println!("Swap instruction accounts: {}", swap_ix_data.accounts.len());
    for (i, acc) in swap_ix_data.accounts.iter().enumerate() {
        println!("  {}: {} (w={}, s={})", i, acc.pubkey, acc.is_writable, acc.is_signer);
    }
    
    // Build transaction
    let recent_blockhash = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer),
        &[&keypair],
        recent_blockhash,
    );
    
    // Simulate
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
    
    println!("\n=== PancakeSwap CLMM swap test complete ===");
    Ok(())
}
