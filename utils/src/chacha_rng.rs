//! ChaCha20-based PRNG, as specified by the ChaCha20-Based PRNG Construction
//! in the Common Cryptographic Components specification.
//!
//! [`ChaCha20Rng`] outputs the raw `ChaCha20` keystream (original variant:
//! 20 rounds, 64-bit counter, 64-bit stream identifier used as a zero nonce),
//! so its output matches any `ChaCha20` implementation keyed with the same
//! 32-byte seed and a zero nonce.

pub use rand::{CryptoRng, RngCore, SeedableRng};
pub use rand_chacha::ChaCha20Rng;

#[cfg(test)]
mod tests {
    use blake2::{Blake2b512, Digest as _};
    use nistrs::prelude::*;

    use super::*;

    const M: usize = 2;

    fn nistrs_rng<Rng: SeedableRng + RngCore>(seed: Rng::Seed) {
        let mut rng = Rng::from_seed(seed);

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

    fn test_seed() -> [u8; 32] {
        let mut hasher = Blake2b512::new();
        hasher.update(
            "Mehmets hope that long srings make it much much much much much much better...",
        );
        let digest: [u8; 64] = hasher.finalize().into();
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&digest[..32]);
        seed
    }

    #[test]
    fn test_nistrs_chacha() {
        println!("======================CHACHA20 RNG========================");
        nistrs_rng::<ChaCha20Rng>(test_seed());
    }

    /// The spec pins the PRNG to the keystream produced by `ChaCha20Rng`;
    /// this guards against a semver bump silently changing the stream.
    #[test]
    fn test_keystream_stability() {
        let mut rng = ChaCha20Rng::from_seed([0u8; 32]);
        let mut out = [0u8; 8];
        rng.fill_bytes(&mut out);
        // First 8 keystream bytes of ChaCha20 under an all-zero key and nonce,
        // per the reference implementation.
        assert_eq!(out, [0x76, 0xb8, 0xe0, 0xad, 0xa0, 0xf1, 0x3d, 0x90]);
    }
}
