use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

fn main() {
    let raydium_clmm = Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").unwrap();
    let pool = Pubkey::from_str("GtKKKs3yaPdHbQd2aZS4SfWhy8zQ988BJGnKNndLxYsN").unwrap();
    
    for start_tick in [68400i32, 64800, 72000] {
        let (pda, _) = Pubkey::find_program_address(
            &[
                b"tick_array",
                pool.as_ref(),
                &start_tick.to_le_bytes(),
            ],
            &raydium_clmm,
        );
        println!("start_tick {}: {}", start_tick, pda);
    }
}
