use std::iter::{Enumerate, FlatMap, Repeat};

use blake2::{Blake2b512, Digest as _};
pub use cipher::StreamCipher;
use cipher::{BlockSizeUser, StreamCipherError, inout::InOutBuf};
use rand::Error;
pub use rand::{CryptoRng, RngCore, SeedableRng};

const OUTPUT_SIZE: usize = 64;
type Hasher = Blake2b512;

#[derive(Clone)]
pub struct BlakeRngSeed([u8; OUTPUT_SIZE]);

impl Default for BlakeRngSeed {
    fn default() -> Self {
        Self([0; OUTPUT_SIZE])
    }
}

impl From<[u8; OUTPUT_SIZE]> for BlakeRngSeed {
    fn from(seed: [u8; OUTPUT_SIZE]) -> Self {
        Self(seed)
    }
}

impl AsRef<[u8]> for BlakeRngSeed {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsMut<[u8]> for BlakeRngSeed {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl IntoIterator for BlakeRngSeed {
    type Item = u8;
    type IntoIter = <[u8; OUTPUT_SIZE] as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

// Box<dyn> is usually more convenient. But for making BlakeRng clone we needed
// the actual type.
type InnerIterator =
    FlatMap<Enumerate<Repeat<BlakeRngSeed>>, Vec<u8>, fn((usize, BlakeRngSeed)) -> Vec<u8>>;

#[derive(Clone)]
pub struct BlakeRng(InnerIterator);

impl BlakeRng {
    fn rehash((i, seed): (usize, BlakeRngSeed)) -> Vec<u8> {
        let mut hasher = Hasher::new();
        hasher.update(seed);
        hasher.update(i.to_le_bytes());
        hasher.finalize().to_vec()
    }

    fn with_seed(seed: BlakeRngSeed) -> Self {
        let iter = std::iter::repeat(seed)
            .enumerate()
            .flat_map(Self::rehash as fn((usize, BlakeRngSeed)) -> Vec<u8>);
        Self(iter)
    }

    fn rng_next_bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut output = [0u8; N];
        self.fill_bytes_(&mut output);
        output
    }

    fn fill_bytes_(&mut self, bytes: &mut [u8]) {
        for entry in bytes.iter_mut() {
            *entry = self.0.next().expect("Iterator is infinite");
        }
    }
}

impl SeedableRng for BlakeRng {
    type Seed = BlakeRngSeed;

    fn from_seed(seed: Self::Seed) -> Self {
        Self::with_seed(seed)
    }
}

impl RngCore for BlakeRng {
    fn next_u32(&mut self) -> u32 {
        u32::from_le_bytes(self.rng_next_bytes::<4>())
    }

    fn next_u64(&mut self) -> u64 {
        u64::from_le_bytes(self.rng_next_bytes::<8>())
    }

    fn fill_bytes(&mut self, dst: &mut [u8]) {
        self.fill_bytes_(dst);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for BlakeRng {}

impl BlockSizeUser for BlakeRng {
    type BlockSize = <Blake2b512 as BlockSizeUser>::BlockSize;
}

impl StreamCipher for BlakeRng {
    fn try_apply_keystream_inout(
        &mut self,
        mut buf: InOutBuf<'_, '_, u8>,
    ) -> Result<(), StreamCipherError> {
        let buff = self.rng_next_bytes::<64>();
        buf.get_out().copy_from_slice(&buff);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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

        // This randomly fails depending on the seed
        // let random_excursions_test_result = random_excursions_test(&data).unwrap();
        // for (i, result) in random_excursions_test_result.iter().enumerate() {
        //     assert!(result.0);
        //     println!("Random excursions test {} P-Value: {}", i, result.1);
        // }
        //
        // let random_excursions_variant_test_result =
        // random_excursions_variant_test(&data).unwrap(); for (i, result) in
        // random_excursions_variant_test_result.iter().enumerate() {
        //     assert!(result.0);
        //     println!("Random excursions variant test {} P-Value: {}", i, result.1);
        // }

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

    fn test_seed() -> [u8; 64] {
        let mut hasher = Hasher::new();
        hasher.update(
            "Mehmets hope that long srings make it much much much much much much better...",
        );
        hasher.finalize().into()
    }
    #[test]
    fn test_nistrs_blake() {
        println!("======================BLAKE RNG==========================");
        nistrs_rng::<BlakeRng>(BlakeRngSeed(test_seed()));
    }

    #[ignore = "This tests is just for comparison with the (blake) above one"]
    #[test]
    fn test_nistrs_chacha() {
        println!("======================CHACHA20 RNG========================");
        let mut seed_buff = [0; 32];
        let seed = test_seed();
        seed_buff.copy_from_slice(&seed[..32]);
        nistrs_rng::<rand_chacha::ChaCha20Rng>(seed_buff);
    }
}
