use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

fn main() {
    let pancake_program = Pubkey::from_str("HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq").unwrap();
    let pool = Pubkey::from_str("Hgnmt4WVNEkDVhu7yQYFKUf9V7DrGdyPCJSuS4pLAG2U").unwrap();
    
    // Known tick array: EpEYL2d5ZVxg4Wtj3JUD9ExaqFLz3UasTSvjr2jtq99X with start=-90600
    let start_tick: i32 = -90600;
    let expected = "EpEYL2d5ZVxg4Wtj3JUD9ExaqFLz3UasTSvjr2jtq99X";
    
    println!("Trying to derive PDA for pool {} start_tick_index = {}", pool, start_tick);
    println!("Expected: {}", expected);
    println!();
    
    let tick_bytes_le = start_tick.to_le_bytes();
    let tick_str = start_tick.to_string();
    
    // Variant 1: "tick_array" + pool + le_bytes
    let seeds1: &[&[u8]] = &[b"tick_array", pool.as_ref(), &tick_bytes_le];
    let (pda1, _) = Pubkey::find_program_address(seeds1, &pancake_program);
    println!("1. [\"tick_array\", pool, le_bytes({:02x?})]", tick_bytes_le);
    println!("   Result: {} {}", pda1, if pda1.to_string() == expected { "✓ MATCH!" } else { "" });
    
    // Variant 2: "tick_array" + pool + string
    let seeds2: &[&[u8]] = &[b"tick_array", pool.as_ref(), tick_str.as_bytes()];
    let (pda2, _) = Pubkey::find_program_address(seeds2, &pancake_program);
    println!("2. [\"tick_array\", pool, string(\"{}\")", tick_str);
    println!("   Result: {} {}", pda2, if pda2.to_string() == expected { "✓ MATCH!" } else { "" });
    
    // Variant 3: pool + "tick_array" + le_bytes
    let seeds3: &[&[u8]] = &[pool.as_ref(), b"tick_array", &tick_bytes_le];
    let (pda3, _) = Pubkey::find_program_address(seeds3, &pancake_program);
    println!("3. [pool, \"tick_array\", le_bytes]");
    println!("   Result: {} {}", pda3, if pda3.to_string() == expected { "✓ MATCH!" } else { "" });
    
    // Variant 4: Just pool + le_bytes
    let seeds4: &[&[u8]] = &[pool.as_ref(), &tick_bytes_le];
    let (pda4, _) = Pubkey::find_program_address(seeds4, &pancake_program);
    println!("4. [pool, le_bytes]");
    println!("   Result: {} {}", pda4, if pda4.to_string() == expected { "✓ MATCH!" } else { "" });
    
    // Variant 5: "tick_array" + le_bytes
    let seeds5: &[&[u8]] = &[b"tick_array", &tick_bytes_le];
    let (pda5, _) = Pubkey::find_program_address(seeds5, &pancake_program);
    println!("5. [\"tick_array\", le_bytes]");
    println!("   Result: {} {}", pda5, if pda5.to_string() == expected { "✓ MATCH!" } else { "" });
    
    // Variant 6: "tick_array" + pool + be_bytes 
    let tick_bytes_be = start_tick.to_be_bytes();
    let seeds6: &[&[u8]] = &[b"tick_array", pool.as_ref(), &tick_bytes_be];
    let (pda6, _) = Pubkey::find_program_address(seeds6, &pancake_program);
    println!("6. [\"tick_array\", pool, be_bytes({:02x?})]", tick_bytes_be);
    println!("   Result: {} {}", pda6, if pda6.to_string() == expected { "✓ MATCH!" } else { "" });
    
    // Variant 7: "tick_array" as different case
    let seeds7: &[&[u8]] = &[b"TickArray", pool.as_ref(), &tick_bytes_le];
    let (pda7, _) = Pubkey::find_program_address(seeds7, &pancake_program);
    println!("7. [\"TickArray\", pool, le_bytes]");
    println!("   Result: {} {}", pda7, if pda7.to_string() == expected { "✓ MATCH!" } else { "" });
    
    // Variant 8: Maybe the seed has spaces or different name
    let seeds8: &[&[u8]] = &[b"tick-array", pool.as_ref(), &tick_bytes_le];
    let (pda8, _) = Pubkey::find_program_address(seeds8, &pancake_program);
    println!("8. [\"tick-array\", pool, le_bytes]");
    println!("   Result: {} {}", pda8, if pda8.to_string() == expected { "✓ MATCH!" } else { "" });
}
