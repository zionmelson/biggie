//! Test Raydium AMM swap instruction
//!
//! Performs a live swap on mainnet to verify the instruction works.
//! Usage: cargo run --bin test_raydium_swap
//!
//! This will:
//! 1. Swap a small amount of SOL → JUP on Raydium AMM
//! 2. Swap the JUP back to SOL
//! 3. Report success/failure

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
use std::sync::LazyLock;
use std::time::Duration;

// =============================================================================
// Constants
// =============================================================================

/// Raydium AMM V4 program
static RAYDIUM_AMM: LazyLock<Pubkey> = LazyLock::new(|| {
    Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap()
});

/// Raydium AMM authority PDA
static RAYDIUM_AMM_AUTHORITY: LazyLock<Pubkey> = LazyLock::new(|| {
    Pubkey::find_program_address(&[b"amm authority"], &RAYDIUM_AMM).0
});

/// Token program
const TOKEN_PROGRAM: Pubkey = solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// ATA program
const ATA_PROGRAM: Pubkey = solana_sdk::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
/// System program
const SYSTEM_PROGRAM: Pubkey = solana_sdk::pubkey!("11111111111111111111111111111111");
/// WSOL mint
const WSOL_MINT: Pubkey = solana_sdk::pubkey!("So11111111111111111111111111111111111111112");
/// JUP mint
const JUP_MINT: Pubkey = solana_sdk::pubkey!("JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN");

/// JUP/SOL Raydium AMM pool from pools.json
/// Address: EYErUp5muPYEEkeaUCY22JibeZX7E9UuMcJFZkmNAN7c
/// token_mint_a: JUP (JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN)
/// token_mint_b: SOL (So11111111111111111111111111111111111111112)
/// token_vault_a: 4xmePoAm93k4KrMzkG7UuVqKnF5WUNUHpXdavXGECaN2
/// token_vault_b: 4nR7HAVv7TDh8mzg7XLbsBnVfgMEGyL7EBq7WuR6ZB16
const JUP_SOL_POOL: Pubkey = solana_sdk::pubkey!("EYErUp5muPYEEkeaUCY22JibeZX7E9UuMcJFZkmNAN7c");
const JUP_SOL_VAULT_A: Pubkey = solana_sdk::pubkey!("4xmePoAm93k4KrMzkG7UuVqKnF5WUNUHpXdavXGECaN2"); // JUP vault
const JUP_SOL_VAULT_B: Pubkey = solana_sdk::pubkey!("4nR7HAVv7TDh8mzg7XLbsBnVfgMEGyL7EBq7WuR6ZB16"); // SOL vault

// Amount to swap: 0.001 SOL (1_000_000 lamports)
const SWAP_AMOUNT_LAMPORTS: u64 = 1_000_000;

// =============================================================================
// Helpers
// =============================================================================

fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM.as_ref(), mint.as_ref()],
        &ATA_PROGRAM,
    )
    .0
}

fn create_ata_idempotent(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    // CreateIdempotent instruction (opcode 1)
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
        // Transfer SOL to wSOL ATA
        solana_sdk::system_instruction::transfer(payer, &wsol_ata, amount),
        // Sync native to update balance
        Instruction {
            program_id: TOKEN_PROGRAM,
            accounts: vec![AccountMeta::new(wsol_ata, false)],
            data: vec![17], // SyncNative
        },
    ]
}

fn close_wsol_account(owner: &Pubkey) -> Instruction {
    let wsol_ata = derive_ata(owner, &WSOL_MINT);
    // CloseAccount instruction
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

/// Build Raydium AMM SwapBaseInV2 instruction
fn raydium_amm_swap_ix(
    pool: Pubkey,
    vault_a: Pubkey,
    vault_b: Pubkey,
    user_source: Pubkey,
    user_dest: Pubkey,
    user: &Pubkey,
    amount_in: u64,
    min_amount_out: u64,
) -> Instruction {
    // SwapBaseInV2 (opcode 16)
    // Accounts:
    // 0. [] Token program
    // 1. [writable] AMM Account
    // 2. [] AMM authority
    // 3. [writable] coin_vault (vault_a)
    // 4. [writable] pc_vault (vault_b)
    // 5. [writable] user_source
    // 6. [writable] user_dest
    // 7. [signer] user

    let accounts = vec![
        AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        AccountMeta::new(pool, false),
        AccountMeta::new_readonly(*RAYDIUM_AMM_AUTHORITY, false),
        AccountMeta::new(vault_a, false),
        AccountMeta::new(vault_b, false),
        AccountMeta::new(user_source, false),
        AccountMeta::new(user_dest, false),
        AccountMeta::new_readonly(*user, true),
    ];

    let mut data = Vec::with_capacity(17);
    data.push(16u8); // SwapBaseInV2
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());

    Instruction {
        program_id: *RAYDIUM_AMM,
        accounts,
        data,
    }
}

// =============================================================================
// Main
// =============================================================================

fn main() -> Result<()> {
    println!("=== Raydium AMM Swap Test ===\n");

    // Load config for RPC endpoint
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
        &std::fs::read_to_string(payer_path)
            .map_err(|e| anyhow!("Failed to read keypair from {}: {}", payer_path, e))?
    )?;
    let payer = Keypair::from_bytes(&payer_bytes)?;

    println!("RPC: {}", rpc_url);
    println!("Payer: {}", payer.pubkey());
    println!("Pool: {} (JUP/SOL)", JUP_SOL_POOL);
    println!("Swap amount: {} lamports (0.001 SOL)\n", SWAP_AMOUNT_LAMPORTS);

    // Create RPC client
    let rpc = RpcClient::new_with_timeout_and_commitment(
        rpc_url.to_string(),
        Duration::from_secs(30),
        CommitmentConfig::confirmed(),
    );

    // Check balance
    let balance = rpc.get_balance(&payer.pubkey())?;
    println!("Current SOL balance: {} lamports ({:.4} SOL)", balance, balance as f64 / 1e9);

    if balance < SWAP_AMOUNT_LAMPORTS + 10_000_000 {
        return Err(anyhow!(
            "Insufficient balance. Need at least {} lamports, have {}",
            SWAP_AMOUNT_LAMPORTS + 10_000_000,
            balance
        ));
    }

    // Derive ATAs
    let wsol_ata = derive_ata(&payer.pubkey(), &WSOL_MINT);
    let jup_ata = derive_ata(&payer.pubkey(), &JUP_MINT);

    println!("wSOL ATA: {}", wsol_ata);
    println!("JUP ATA: {}", jup_ata);

    // Check initial JUP balance
    let initial_jup_balance = match rpc.get_token_account_balance(&jup_ata) {
        Ok(b) => b.amount.parse::<u64>().unwrap_or(0),
        Err(_) => 0,
    };
    println!("Initial JUP balance: {} (raw)\n", initial_jup_balance);

    // =========================================================================
    // SWAP 1: SOL → JUP
    // =========================================================================
    println!("--- Swap 1: SOL → JUP ---");

    let mut instructions = vec![];

    // Compute budget
    instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(200_000));
    instructions.push(ComputeBudgetInstruction::set_compute_unit_price(50_000)); // Priority fee

    // Create ATAs (idempotent)
    instructions.push(create_ata_idempotent(&payer.pubkey(), &payer.pubkey(), &WSOL_MINT));
    instructions.push(create_ata_idempotent(&payer.pubkey(), &payer.pubkey(), &JUP_MINT));

    // Wrap SOL
    instructions.extend(wrap_sol(&payer.pubkey(), SWAP_AMOUNT_LAMPORTS));

    // Swap: SOL (vault_b) → JUP (vault_a)
    // In this pool: mint_a = JUP, mint_b = SOL
    // So swapping SOL→JUP means: source = wSOL ATA, dest = JUP ATA
    instructions.push(raydium_amm_swap_ix(
        JUP_SOL_POOL,
        JUP_SOL_VAULT_A, // JUP vault (coin)
        JUP_SOL_VAULT_B, // SOL vault (pc)
        wsol_ata,        // source (we're sending SOL)
        jup_ata,         // dest (we're receiving JUP)
        &payer.pubkey(),
        SWAP_AMOUNT_LAMPORTS,
        1, // min_out = 1 (accept any amount for test)
    ));

    // Close wSOL account to recover rent
    instructions.push(close_wsol_account(&payer.pubkey()));

    // Build and sign transaction
    let blockhash = rpc.get_latest_blockhash()?;
    let message = Message::new(&instructions, Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(message);
    tx.sign(&[&payer], blockhash);

    println!("Sending swap 1 transaction...");
    match rpc.send_and_confirm_transaction_with_spinner(&tx) {
        Ok(sig) => {
            println!("✓ Swap 1 SUCCESS: https://solscan.io/tx/{}", sig);
        }
        Err(e) => {
            println!("✗ Swap 1 FAILED: {:?}", e);
            return Err(anyhow!("Swap 1 failed: {:?}", e));
        }
    }

    // Check JUP balance after swap 1
    std::thread::sleep(Duration::from_secs(2));
    let jup_balance_after_swap1 = match rpc.get_token_account_balance(&jup_ata) {
        Ok(b) => b.amount.parse::<u64>().unwrap_or(0),
        Err(e) => return Err(anyhow!("Failed to get JUP balance: {:?}", e)),
    };
    let jup_received = jup_balance_after_swap1.saturating_sub(initial_jup_balance);
    println!("JUP received: {} (raw, 6 decimals = {} JUP)\n", jup_received, jup_received as f64 / 1e6);

    if jup_received == 0 {
        return Err(anyhow!("Swap 1 returned 0 JUP - something went wrong"));
    }

    // =========================================================================
    // SWAP 2: JUP → SOL
    // =========================================================================
    println!("--- Swap 2: JUP → SOL ---");

    let mut instructions2 = vec![];

    // Compute budget
    instructions2.push(ComputeBudgetInstruction::set_compute_unit_limit(200_000));
    instructions2.push(ComputeBudgetInstruction::set_compute_unit_price(50_000));

    // Create wSOL ATA (idempotent)
    instructions2.push(create_ata_idempotent(&payer.pubkey(), &payer.pubkey(), &WSOL_MINT));

    // Swap: JUP (vault_a) → SOL (vault_b)
    // source = JUP ATA, dest = wSOL ATA
    instructions2.push(raydium_amm_swap_ix(
        JUP_SOL_POOL,
        JUP_SOL_VAULT_A, // JUP vault (coin)
        JUP_SOL_VAULT_B, // SOL vault (pc)
        jup_ata,         // source (we're sending JUP)
        wsol_ata,        // dest (we're receiving SOL)
        &payer.pubkey(),
        jup_received,    // swap all the JUP we received
        1,               // min_out = 1 (accept any amount for test)
    ));

    // Close wSOL account to unwrap to native SOL
    instructions2.push(close_wsol_account(&payer.pubkey()));

    // Build and sign transaction
    let blockhash2 = rpc.get_latest_blockhash()?;
    let message2 = Message::new(&instructions2, Some(&payer.pubkey()));
    let mut tx2 = Transaction::new_unsigned(message2);
    tx2.sign(&[&payer], blockhash2);

    println!("Sending swap 2 transaction...");
    match rpc.send_and_confirm_transaction_with_spinner(&tx2) {
        Ok(sig) => {
            println!("✓ Swap 2 SUCCESS: https://solscan.io/tx/{}", sig);
        }
        Err(e) => {
            println!("✗ Swap 2 FAILED: {:?}", e);
            return Err(anyhow!("Swap 2 failed: {:?}", e));
        }
    }

    // Final balance check
    std::thread::sleep(Duration::from_secs(2));
    let final_balance = rpc.get_balance(&payer.pubkey())?;
    let sol_lost = balance.saturating_sub(final_balance);
    
    println!("\n=== Summary ===");
    println!("Initial SOL: {} lamports", balance);
    println!("Final SOL:   {} lamports", final_balance);
    println!("Net loss:    {} lamports ({:.6} SOL) - covers fees + slippage", sol_lost, sol_lost as f64 / 1e9);
    println!("\n✓ Both swaps completed successfully!");
    println!("  The Raydium AMM SwapBaseInV2 instruction is working correctly.");

    Ok(())
}
