//! Statistical and stability tests for the ChaCha20-based PRNG, as specified
//! by the ChaCha20-Based PRNG Construction in the Common Cryptographic
//! Components specification.

use logos_blockchain_blend_crypto::{blake2b512, chacha20_rng};
use nistrs::prelude::*;
use rand::RngCore as _;

const M: usize = 2;

fn nistrs_rng(seed: &[u8; 64]) {
    let mut rng = chacha20_rng(seed);

    let mut buffer = vec![0u8; 1_000_000];
    rng.fill_bytes(&mut buffer);

    let data = BitsData::from_binary(buffer);

    let approximate_entropy_test_result = approximate_entropy_test(&data, M);
    assert!(approximate_entropy_test_result.0);
    println!(
        "Approximate entropy test P-Value: {}",
        approximate_entropy_test_result.1
    );

    let block_frequency_test_result = block_frequency_test(&data, M).unwrap();
    assert!(block_frequency_test_result.0);
    println!(
        "Block frequency test P-Value: {}",
        block_frequency_test_result.1
    );

    let cumulative_sums_test_result = cumulative_sums_test(&data);
    assert!(cumulative_sums_test_result[0].0);
    assert!(cumulative_sums_test_result[1].0);
    println!(
        "Cumulative sums forward test P-Value: {}",
        cumulative_sums_test_result[0].1
    );
    println!(
        "Cumulative sums backward test P-Value: {}",
        cumulative_sums_test_result[1].1
    );

    let fft_test_result = fft_test(&data);
    assert!(fft_test_result.0);
    println!("FFT test P-Value: {}", fft_test_result.1);

    let frequency_test_result = frequency_test(&data);
    assert!(frequency_test_result.0);
    println!("Frequency test P-Value: {}", frequency_test_result.1);

    let linear_complexity_test_result = linear_complexity_test(&data, 64);
    assert!(linear_complexity_test_result.0);
    println!(
        "Linear complexity test P-Value: {}",
        linear_complexity_test_result.1
    );

    let longest_run_test_result = longest_run_of_ones_test(&data).unwrap();
    assert!(longest_run_test_result.0);
    println!("Longest run test P-Value: {}", longest_run_test_result.1);

    let non_overlapping_template_test_result = non_overlapping_template_test(&data, M).unwrap();
    for (i, result) in non_overlapping_template_test_result.iter().enumerate() {
        assert!(result.0);
        println!("Non-overlapping template test {} P-Value: {}", i, result.1);
    }

    let overlapping_template_test_result = overlapping_template_test(&data, M);
    assert!(overlapping_template_test_result.0);
    println!(
        "Overlapping template test P-Value: {}",
        overlapping_template_test_result.1
    );

    let rank_test_result = rank_test(&data).unwrap();
    assert!(rank_test_result.0);
    println!("Rank test P-Value: {}", rank_test_result.1);

    let runs_test_result = runs_test(&data);
    assert!(runs_test_result.0);
    println!("Runs test P-Value: {}", runs_test_result.1);

    let serial_test_result = serial_test(&data, M);
    assert!(serial_test_result[0].0);
    assert!(serial_test_result[1].0);
    println!("Serial test 1 P-Value: {}", serial_test_result[0].1);
    println!("Serial test 2 P-Value: {}", serial_test_result[1].1);

    let universal_test_result = universal_test(&data);
    assert!(universal_test_result.0);
    println!("Universal test P-Value: {}", universal_test_result.1);
}

#[test]
fn test_nistrs_chacha() {
    println!("======================CHACHA20 RNG========================");
    nistrs_rng(&blake2b512(&[
        b"Mehmets hope that long srings make it much much much much much much better...",
    ]));
}

/// The spec pins the PRNG to the `ChaCha20` keystream; this guards against a
/// dependency bump silently changing the stream.
#[test]
fn test_keystream_stability() {
    let mut rng = chacha20_rng(&[0u8; 64]);
    let mut out = [0u8; 8];
    rng.fill_bytes(&mut out);
    // First 8 keystream bytes of ChaCha20 under an all-zero key and nonce,
    // per the reference implementation.
    assert_eq!(out, [0x76, 0xb8, 0xe0, 0xad, 0xa0, 0xf1, 0x3d, 0x90]);
}

/// Only the first 32 bytes of the 64-byte seed key the stream.
#[test]
fn test_seed_truncation() {
    let mut seed = [0u8; 64];
    let mut base = [0u8; 8];
    chacha20_rng(&seed).fill_bytes(&mut base);

    seed[32] = 0xFF;
    let mut upper_half_changed = [0u8; 8];
    chacha20_rng(&seed).fill_bytes(&mut upper_half_changed);
    assert_eq!(base, upper_half_changed);

    seed[0] = 0xFF;
    let mut key_changed = [0u8; 8];
    chacha20_rng(&seed).fill_bytes(&mut key_changed);
    assert_ne!(base, key_changed);
}
