//! Test Meteora DLMM swap instruction
//!
//! Performs a live swap on mainnet to verify the instruction works.
//! Usage: cargo run --release --bin test_meteora_dlmm
//!
//! This will:
//! 1. Parse the LbPair state to get active_bin_id, bin_step, etc.
//! 2. Derive oracle and bin array PDAs
//! 3. Swap a small amount of SOL → BONK on Meteora DLMM
//! 4. Swap the BONK back to SOL
//! 5. Report success/failure

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

// =============================================================================
// Constants
// =============================================================================

/// Meteora DLMM program
const DLMM_PROGRAM: Pubkey = solana_sdk::pubkey!("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");

/// Token programs
const TOKEN_PROGRAM: Pubkey = solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATA_PROGRAM: Pubkey = solana_sdk::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const SYSTEM_PROGRAM: Pubkey = solana_sdk::pubkey!("11111111111111111111111111111111");
const MEMO_PROGRAM: Pubkey = solana_sdk::pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

/// Mints
const WSOL_MINT: Pubkey = solana_sdk::pubkey!("So11111111111111111111111111111111111111112");
const BONK_MINT: Pubkey = solana_sdk::pubkey!("DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263");

/// SOL/BONK Meteora DLMM pool from pools.json
/// This is a real DLMM pool for testing
const SOL_BONK_DLMM_POOL: Pubkey = solana_sdk::pubkey!("7eexH14UjhNxJe6zTT3f1Vb1E8iACsBMVaWheDEmxdT2");
// Vaults will be derived from pool state

/// swap2 discriminator: [65, 75, 63, 76, 235, 91, 91, 136]
const SWAP2_DISCRIMINATOR: [u8; 8] = [0x41, 0x4B, 0x3F, 0x4C, 0xEB, 0x5B, 0x5B, 0x88];

/// LbPair struct offsets (bytemuck C-aligned)
/// Based on IDL analysis:
/// - StaticParameters: 32 bytes (offset 0-31)
/// - VariableParameters: 32 bytes (offset 32-63)
/// - bumpSeed: 1 byte (offset 64)
/// - binStepSeed: 2 bytes (offset 65-66)
/// - pairType: 1 byte (offset 67)
/// - activeId: 4 bytes i32 (offset 68-71)
/// - binStep: 2 bytes u16 (offset 72-73)
/// - status: 1 byte (offset 74)
/// - requireBaseFactorSeed: 1 byte (offset 75)
/// - baseFactorSeed: 2 bytes (offset 76-77)
/// - activationType: 1 byte (offset 78)
/// - creatorPoolOnOffControl: 1 byte (offset 79)
/// - tokenXMint: 32 bytes (offset 80-111)
/// - tokenYMint: 32 bytes (offset 112-143)
/// - reserveX: 32 bytes (offset 144-175)
/// - reserveY: 32 bytes (offset 176-207)
/// - protocolFee: 16 bytes (offset 208-223)
/// - padding1: 32 bytes (offset 224-255)
/// - rewardInfos: 256 bytes (offset 256-511)
/// - oracle: 32 bytes (offset 512-543)
/// - binArrayBitmap: 128 bytes (offset 544-671)

const LBPAIR_ACTIVE_ID_OFFSET: usize = 68;
const LBPAIR_BIN_STEP_OFFSET: usize = 72;
const LBPAIR_STATUS_OFFSET: usize = 74;
const LBPAIR_TOKEN_X_MINT_OFFSET: usize = 80;
const LBPAIR_TOKEN_Y_MINT_OFFSET: usize = 112;
const LBPAIR_RESERVE_X_OFFSET: usize = 144;
const LBPAIR_RESERVE_Y_OFFSET: usize = 176;
const LBPAIR_ORACLE_OFFSET: usize = 512;
const LBPAIR_BIN_ARRAY_BITMAP_OFFSET: usize = 544;

/// Bins per bin array
const BINS_PER_ARRAY: i32 = 70;

/// Amount to swap: 0.001 SOL
const SWAP_AMOUNT_LAMPORTS: u64 = 1_000_000;

// =============================================================================
// LbPair Parsing
// =============================================================================

#[derive(Debug)]
struct LbPairState {
    active_id: i32,
    bin_step: u16,
    status: u8,
    token_x_mint: Pubkey,
    token_y_mint: Pubkey,
    reserve_x: Pubkey,
    reserve_y: Pubkey,
    oracle: Pubkey,
    bin_array_bitmap: [u64; 16],
}

fn parse_lbpair(data: &[u8]) -> Result<LbPairState> {
    if data.len() < 700 {
        return Err(anyhow!("LbPair data too short: {} bytes", data.len()));
    }

    // Skip 8-byte Anchor discriminator
    let data = &data[8..];

    let active_id = i32::from_le_bytes(
        data[LBPAIR_ACTIVE_ID_OFFSET..LBPAIR_ACTIVE_ID_OFFSET + 4]
            .try_into()
            .unwrap(),
    );

    let bin_step = u16::from_le_bytes(
        data[LBPAIR_BIN_STEP_OFFSET..LBPAIR_BIN_STEP_OFFSET + 2]
            .try_into()
            .unwrap(),
    );

    let status = data[LBPAIR_STATUS_OFFSET];

    let token_x_mint = Pubkey::try_from(&data[LBPAIR_TOKEN_X_MINT_OFFSET..LBPAIR_TOKEN_X_MINT_OFFSET + 32])?;
    let token_y_mint = Pubkey::try_from(&data[LBPAIR_TOKEN_Y_MINT_OFFSET..LBPAIR_TOKEN_Y_MINT_OFFSET + 32])?;
    let reserve_x = Pubkey::try_from(&data[LBPAIR_RESERVE_X_OFFSET..LBPAIR_RESERVE_X_OFFSET + 32])?;
    let reserve_y = Pubkey::try_from(&data[LBPAIR_RESERVE_Y_OFFSET..LBPAIR_RESERVE_Y_OFFSET + 32])?;
    let oracle = Pubkey::try_from(&data[LBPAIR_ORACLE_OFFSET..LBPAIR_ORACLE_OFFSET + 32])?;

    let mut bin_array_bitmap = [0u64; 16];
    for i in 0..16 {
        let offset = LBPAIR_BIN_ARRAY_BITMAP_OFFSET + i * 8;
        bin_array_bitmap[i] = u64::from_le_bytes(
            data[offset..offset + 8].try_into().unwrap(),
        );
    }

    Ok(LbPairState {
        active_id,
        bin_step,
        status,
        token_x_mint,
        token_y_mint,
        reserve_x,
        reserve_y,
        oracle,
        bin_array_bitmap,
    })
}

// =============================================================================
// PDA Derivations
// =============================================================================

/// Derive oracle PDA: ["oracle", lb_pair]
fn derive_oracle_pda(lb_pair: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"oracle", lb_pair.as_ref()], &DLMM_PROGRAM).0
}

/// Derive bin array PDA: ["bin_array", lb_pair, bin_array_index.to_le_bytes()]
fn derive_bin_array_pda(lb_pair: &Pubkey, bin_array_index: i64) -> Pubkey {
    Pubkey::find_program_address(
        &[b"bin_array", lb_pair.as_ref(), &bin_array_index.to_le_bytes()],
        &DLMM_PROGRAM,
    ).0
}

/// Derive event authority PDA: ["__event_authority"]
fn derive_event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &DLMM_PROGRAM).0
}

/// Calculate bin array index from bin id
fn bin_id_to_array_index(bin_id: i32) -> i64 {
    // Integer division that rounds toward negative infinity
    if bin_id >= 0 {
        (bin_id / BINS_PER_ARRAY) as i64
    } else {
        ((bin_id + 1) / BINS_PER_ARRAY - 1) as i64
    }
}

/// Find bin arrays needed for a swap based on active bin and direction
fn find_bin_arrays_for_swap(
    rpc: &RpcClient,
    lb_pair: &Pubkey,
    active_id: i32,
    swap_for_y: bool, // true = X→Y (price goes down, bin_id decreases)
) -> Result<Vec<Pubkey>> {
    let current_array_idx = bin_id_to_array_index(active_id);
    
    println!("Active bin ID: {}, Current array index: {}", active_id, current_array_idx);
    println!("swap_for_y: {} ({})", swap_for_y, if swap_for_y { "X→Y, bins go DOWN" } else { "Y→X, bins go UP" });

    // Generate candidate array indices
    let indices: Vec<i64> = if swap_for_y {
        // X→Y: Price goes down, we need current and lower arrays
        (-3..=0).map(|offset| current_array_idx + offset).collect()
    } else {
        // Y→X: Price goes up, we need current and higher arrays
        (0..=3).map(|offset| current_array_idx + offset).collect()
    };

    // Derive PDAs for all candidates
    let pdas: Vec<Pubkey> = indices.iter()
        .map(|&idx| derive_bin_array_pda(lb_pair, idx))
        .collect();

    println!("Checking {} bin array PDAs...", pdas.len());

    // Check which ones exist
    let accounts = rpc.get_multiple_accounts(&pdas)?;
    
    let existing: Vec<Pubkey> = accounts.iter()
        .zip(pdas.iter())
        .zip(indices.iter())
        .filter_map(|((maybe_acc, &pda), &idx)| {
            if maybe_acc.is_some() {
                println!("  Found bin array at index {}: {}", idx, pda);
                Some(pda)
            } else {
                None
            }
        })
        .collect();

    if existing.is_empty() {
        return Err(anyhow!("No bin arrays found for swap!"));
    }

    Ok(existing)
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

/// Build Meteora DLMM swap2 instruction
///
/// Account order (from IDL):
/// 0:  lbPair (writable)
/// 1:  binArrayBitmapExtension (optional, can be program ID)
/// 2:  reserveX (writable)
/// 3:  reserveY (writable)
/// 4:  userTokenIn (writable)
/// 5:  userTokenOut (writable)
/// 6:  tokenXMint
/// 7:  tokenYMint
/// 8:  oracle (writable)
/// 9:  hostFeeIn (optional, can be program ID)
/// 10: user (signer)
/// 11: tokenXProgram
/// 12: tokenYProgram
/// 13: memoProgram
/// 14: eventAuthority
/// 15: program (DLMM program ID)
/// 16+: binArrays (remaining accounts)
fn dlmm_swap2_ix(
    lb_pair: &Pubkey,
    reserve_x: &Pubkey,
    reserve_y: &Pubkey,
    user_token_in: &Pubkey,
    user_token_out: &Pubkey,
    token_x_mint: &Pubkey,
    token_y_mint: &Pubkey,
    oracle: &Pubkey,
    user: &Pubkey,
    bin_arrays: &[Pubkey],
    amount_in: u64,
    min_amount_out: u64,
    swap_for_y: bool, // true = X→Y, false = Y→X
) -> Instruction {
    let event_authority = derive_event_authority();

    let mut accounts = vec![
        AccountMeta::new(*lb_pair, false),           // 0: lbPair
        AccountMeta::new_readonly(DLMM_PROGRAM, false), // 1: binArrayBitmapExtension (optional - use program as placeholder)
        AccountMeta::new(*reserve_x, false),         // 2: reserveX
        AccountMeta::new(*reserve_y, false),         // 3: reserveY
        AccountMeta::new(*user_token_in, false),     // 4: userTokenIn
        AccountMeta::new(*user_token_out, false),    // 5: userTokenOut
        AccountMeta::new_readonly(*token_x_mint, false), // 6: tokenXMint
        AccountMeta::new_readonly(*token_y_mint, false), // 7: tokenYMint
        AccountMeta::new(*oracle, false),            // 8: oracle
        AccountMeta::new_readonly(DLMM_PROGRAM, false), // 9: hostFeeIn (optional - use program as placeholder)
        AccountMeta::new_readonly(*user, true),      // 10: user (signer)
        AccountMeta::new_readonly(TOKEN_PROGRAM, false), // 11: tokenXProgram
        AccountMeta::new_readonly(TOKEN_PROGRAM, false), // 12: tokenYProgram
        AccountMeta::new_readonly(MEMO_PROGRAM, false),  // 13: memoProgram
        AccountMeta::new_readonly(event_authority, false), // 14: eventAuthority
        AccountMeta::new_readonly(DLMM_PROGRAM, false),    // 15: program
    ];

    // Add bin arrays as remaining accounts
    for bin_array in bin_arrays {
        accounts.push(AccountMeta::new(*bin_array, false));
    }

    // Build instruction data:
    // - discriminator: 8 bytes
    // - amount_in: u64 (8 bytes)
    // - min_amount_out: u64 (8 bytes)
    // - remaining_accounts_info: serialized struct
    //
    // RemainingAccountsInfo contains:
    // - slices: Vec<RemainingAccountsSlice>
    //   Each slice has: accounts_type (enum) + length (u8)
    //
    // For a simple swap with only bin arrays, we need to encode the bin arrays as a slice

    let mut data = Vec::with_capacity(50);
    data.extend_from_slice(&SWAP2_DISCRIMINATOR);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());
    
    // RemainingAccountsInfo encoding:
    // This is a Vec<RemainingAccountsSlice> encoded by Borsh
    // For just bin arrays, we can pass an empty vec since bin arrays are
    // the default remaining accounts without special typing
    // Vec length = 0 (4 bytes for borsh Vec length)
    data.extend_from_slice(&0u32.to_le_bytes()); // Empty slices vec

    Instruction {
        program_id: DLMM_PROGRAM,
        accounts,
        data,
    }
}

// =============================================================================
// Main
// =============================================================================

fn main() -> Result<()> {
    println!("=== Meteora DLMM Swap Test ===\n");

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
    
    let wallet_bytes: Vec<u8> = serde_json::from_str(
        &std::fs::read_to_string(payer_path)?
    )?;
    let keypair = Keypair::from_bytes(&wallet_bytes)?;
    let payer = keypair.pubkey();
    
    println!("RPC: {}", rpc_url);
    println!("Payer: {}", payer);
    println!("Pool: {} (SOL/BONK DLMM)", SOL_BONK_DLMM_POOL);
    println!("Swap amount: {} lamports (0.001 SOL)\n", SWAP_AMOUNT_LAMPORTS);

    let rpc = RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed());

    // Check balance
    let balance = rpc.get_balance(&payer)?;
    println!("Balance: {} SOL", balance as f64 / 1e9);
    if balance < SWAP_AMOUNT_LAMPORTS * 2 {
        return Err(anyhow!("Insufficient balance for test"));
    }

    // Fetch and parse LbPair state
    println!("\n--- Fetching LbPair State ---");
    let lb_pair_account = rpc.get_account(&SOL_BONK_DLMM_POOL)?;
    println!("LbPair data length: {} bytes", lb_pair_account.data.len());
    
    let lb_pair_state = parse_lbpair(&lb_pair_account.data)?;
    println!("Active bin ID: {}", lb_pair_state.active_id);
    println!("Bin step: {}", lb_pair_state.bin_step);
    println!("Status: {}", lb_pair_state.status);
    println!("Token X mint: {}", lb_pair_state.token_x_mint);
    println!("Token Y mint: {}", lb_pair_state.token_y_mint);
    println!("Reserve X: {}", lb_pair_state.reserve_x);
    println!("Reserve Y: {}", lb_pair_state.reserve_y);
    println!("Oracle: {}", lb_pair_state.oracle);

    // Use derived oracle PDA (the on-chain field may be zeroed for some pools)
    let oracle = derive_oracle_pda(&SOL_BONK_DLMM_POOL);
    println!("Oracle PDA: {}", oracle);

    // Derive user ATAs
    let user_wsol_ata = derive_ata(&payer, &WSOL_MINT);
    let user_bonk_ata = derive_ata(&payer, &BONK_MINT);
    println!("\nUser WSOL ATA: {}", user_wsol_ata);
    println!("User BONK ATA: {}", user_bonk_ata);

    // Determine swap direction
    // Pool is SOL/BONK, so:
    // - If token_x = SOL and token_y = BONK: SOL→BONK is swap_for_y=true
    // - If token_x = BONK and token_y = SOL: SOL→BONK is swap_for_y=false
    let sol_is_x = lb_pair_state.token_x_mint == WSOL_MINT;
    println!("\nSOL is token_x: {}", sol_is_x);

    // For SOL → BONK swap
    let swap_for_y = sol_is_x; // If SOL is X, we're swapping X for Y
    println!("For SOL→BONK: swap_for_y = {}", swap_for_y);

    // Find bin arrays for swap
    println!("\n--- Finding Bin Arrays ---");
    let bin_arrays = find_bin_arrays_for_swap(
        &rpc,
        &SOL_BONK_DLMM_POOL,
        lb_pair_state.active_id,
        swap_for_y,
    )?;
    println!("Found {} bin arrays for swap", bin_arrays.len());

    // Build instructions
    println!("\n--- Building Transaction ---");
    let mut instructions = vec![
        // Set compute budget
        ComputeBudgetInstruction::set_compute_unit_limit(400_000),
        ComputeBudgetInstruction::set_compute_unit_price(50_000),
        // Create ATAs if needed
        create_ata_idempotent(&payer, &payer, &WSOL_MINT),
        create_ata_idempotent(&payer, &payer, &BONK_MINT),
    ];

    // Wrap SOL
    instructions.extend(wrap_sol(&payer, SWAP_AMOUNT_LAMPORTS));

    // Determine input/output based on direction
    let (user_token_in, user_token_out, reserve_in, reserve_out) = if swap_for_y {
        // SOL → BONK (X → Y)
        (user_wsol_ata, user_bonk_ata, lb_pair_state.reserve_x, lb_pair_state.reserve_y)
    } else {
        // This shouldn't happen for SOL→BONK but handle it
        (user_bonk_ata, user_wsol_ata, lb_pair_state.reserve_y, lb_pair_state.reserve_x)
    };

    // DLMM swap
    let swap_ix = dlmm_swap2_ix(
        &SOL_BONK_DLMM_POOL,
        &lb_pair_state.reserve_x,
        &lb_pair_state.reserve_y,
        &user_token_in,
        &user_token_out,
        &lb_pair_state.token_x_mint,
        &lb_pair_state.token_y_mint,
        &oracle,  // Use derived oracle PDA
        &payer,
        &bin_arrays,
        SWAP_AMOUNT_LAMPORTS,
        1, // min_amount_out = 1 (accept any output for testing)
        swap_for_y,
    );
    instructions.push(swap_ix);

    // Close WSOL account to recover rent
    instructions.push(close_wsol_account(&payer));

    // Build and send transaction
    let blockhash = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer),
        &[&keypair],
        blockhash,
    );

    println!("Sending transaction...");
    match rpc.send_and_confirm_transaction(&tx) {
        Ok(sig) => {
            println!("\n✅ SOL → BONK swap SUCCESS!");
            println!("Signature: {}", sig);
            println!("Explorer: https://solscan.io/tx/{}", sig);
        }
        Err(e) => {
            println!("\n❌ SOL → BONK swap FAILED!");
            println!("Error: {:?}", e);
            
            // Try to simulate to get more info
            println!("\nSimulating to get error details...");
            match rpc.simulate_transaction(&tx) {
                Ok(sim) => {
                    println!("Simulation result: {:?}", sim.value);
                    if let Some(logs) = sim.value.logs {
                        println!("\nLogs:");
                        for log in logs {
                            println!("  {}", log);
                        }
                    }
                }
                Err(sim_err) => {
                    println!("Simulation error: {:?}", sim_err);
                }
            }
            return Err(anyhow!("Swap failed: {:?}", e));
        }
    }

    Ok(())
}
