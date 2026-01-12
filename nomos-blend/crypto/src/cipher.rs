use nomos_utils::blake_rng::{BlakeRng, RngCore as _, SeedableRng as _};

use crate::blake2b512;

/// A cipher that encrypts/decrypts data using XOR with pseudo-random bytes.
///
/// The cipher is initialized with a seed, and produces a deterministic stream
/// of pseudo-random bytes derived from the seed for encryption/decryption.
/// Because the byte stream is deterministic, any data sequence encrypted with
/// a cipher initialized from a given seed can be decrypted by another cipher
/// created from the same seed, as long as the data sequence is consumed in the
/// same order.
pub struct Cipher(BlakeRng);

impl Cipher {
    #[must_use]
    pub(crate) fn new(domain: &[u8], seed: &[u8]) -> Self {
        Self(BlakeRng::from_seed(blake2b512(&[domain, seed]).into()))
    }

    /// Encrypts data in-place by XOR operation with a pseudo-random bytes.
    pub fn encrypt(&mut self, data: &mut [u8]) {
        Self::xor_in_place(data, &self.next_pseudo_random_bytes(data.len()));
    }

    /// Decrypts data in-place by XOR operation with a pseudo-random bytes.
    pub fn decrypt(&mut self, data: &mut [u8]) {
        self.encrypt(data); // encryption and decryption are symmetric.
    }

    /// XORs two byte slices in-place.
    fn xor_in_place(a: &mut [u8], b: &[u8]) {
        assert_eq!(a.len(), b.len());
        a.iter_mut().zip(b.iter()).for_each(|(x1, &x2)| *x1 ^= x2);
    }

    /// Generates the next `size` pseudo-random bytes.
    fn next_pseudo_random_bytes(&mut self, size: usize) -> Vec<u8> {
        let mut buf = vec![0u8; size];
        self.0.fill_bytes(&mut buf);
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_DOMAIN: &[u8] = b"test-domain";

    #[test]
    fn cipher_deterministic_pseudo_random_bytes() {
        let seed = b"test-seed";
        let length = 64;

        let mut cipher = Cipher::new(TEST_DOMAIN, seed);
        let bytes1 = cipher.next_pseudo_random_bytes(length);
        assert_eq!(bytes1.len(), length);
        let bytes2 = cipher.next_pseudo_random_bytes(length);
        assert_eq!(bytes2.len(), length);
        assert_ne!(bytes1, bytes2);

        let mut cipher = Cipher::new(TEST_DOMAIN, seed);
        assert_eq!(cipher.next_pseudo_random_bytes(length), bytes1);
        assert_eq!(cipher.next_pseudo_random_bytes(length), bytes2);

        let mut cipher = Cipher::new(TEST_DOMAIN, b"different-seed");
        assert_ne!(cipher.next_pseudo_random_bytes(length), bytes1);
        assert_ne!(cipher.next_pseudo_random_bytes(length), bytes2);
    }

    #[test]
    fn cipher_encrypt_decrypt() {
        let seed = b"test-seed";
        let mut data1 = b"hello".to_vec();
        let mut data2 = b"world".to_vec();

        let mut cipher = Cipher::new(TEST_DOMAIN, seed);
        cipher.encrypt(&mut data1);
        cipher.encrypt(&mut data2);

        let mut cipher = Cipher::new(TEST_DOMAIN, seed);
        cipher.decrypt(&mut data1);
        assert_eq!(&data1, b"hello");
        cipher.decrypt(&mut data2);
        assert_eq!(&data2, b"world");
    }

    #[test]
    fn cipher_encrypt_decrypt_with_different_seed() {
        let mut data1 = b"hello".to_vec();
        let mut data2 = b"world".to_vec();

        let mut cipher = Cipher::new(TEST_DOMAIN, b"test-seed");
        cipher.encrypt(&mut data1);
        cipher.encrypt(&mut data2);

        let mut cipher = Cipher::new(TEST_DOMAIN, b"different-seed");
        cipher.decrypt(&mut data1);
        assert_ne!(&data1, b"hello");
        cipher.decrypt(&mut data2);
        assert_ne!(&data2, b"world");
    }

    #[test]
    fn xor_leakage_security() {
        let plain1 = b"hello".to_vec();
        let plain2 = b"world".to_vec();

        let mut cipher = Cipher::new(TEST_DOMAIN, b"test-seed");
        let encrypted1 = encrypt(&plain1, &mut cipher);
        let encrypted2 = encrypt(&plain2, &mut cipher);

        // XOR the two ciphertexts
        let xor_two_ciphers = xor(&encrypted1, &encrypted2);
        // XOR the two plaintexts
        let xor_two_plains = xor(&plain1, &plain2);

        // xor_two_plains and xor_two_ciphers shouldn't be the same.
        // because `Cipher` advances PRNG at each encryption.
        assert_ne!(xor_two_ciphers, xor_two_plains);

        // Even if someone knows `plain1` "somehow" (while not knowing the seed),
        // he can't recover `plain2` in the following way.
        let leaked_plain2 = xor(&plain1, &xor_two_ciphers);
        assert_ne!(leaked_plain2, plain2);
    }

    #[test]
    fn xor_leakage_security_in_encapsulation() {
        let plain1 = b"hello".to_vec();
        let plain2 = b"world".to_vec();

        let seed1 = b"seed1";
        let seed2 = b"seed2";

        // First, encrypt `plain2` with the `seed2`.
        let mut cipher2 = Cipher::new(TEST_DOMAIN, seed2);
        let encrypted2 = encrypt(&plain2, &mut cipher2);

        // Second, encrypt `plain1` and `encrypted2` with the `seed1`.
        let mut cipher1 = Cipher::new(TEST_DOMAIN, seed1);
        let encrypted1 = encrypt(&plain1, &mut cipher1);
        let double_encrypted2 = encrypt(&encrypted2, &mut cipher1);

        // XOR `encrypted1` and `double_encrypted2`.
        let xor_encrypted1_and_double_encrypted2 = xor(&encrypted1, &double_encrypted2);

        // Now, someone who knows `seed1` can recover `plain1` and `encrypted2`
        // (not `plain2`). This is the intended use case.
        let mut cipher1 = Cipher::new(TEST_DOMAIN, seed1);
        let recovered_plain1 = decrypt(&encrypted1, &mut cipher1);
        assert_eq!(recovered_plain1, plain1);
        let recovered_encrypted2 = decrypt(&double_encrypted2, &mut cipher1);
        assert_eq!(recovered_encrypted2, encrypted2);

        // However,
        // even if someone knows `plain1` "somehow" (while not knowing `seed1`),
        // he can't recover `encrypted2` in the following way.
        let leaked_encrypted2 = xor(&plain1, &xor_encrypted1_and_double_encrypted2);
        assert_ne!(leaked_encrypted2, encrypted2);
    }

    fn encrypt(data: &[u8], cipher: &mut Cipher) -> Vec<u8> {
        let mut buf = data.to_vec();
        cipher.encrypt(&mut buf);
        buf
    }

    fn decrypt(data: &[u8], cipher: &mut Cipher) -> Vec<u8> {
        let mut buf = data.to_vec();
        cipher.decrypt(&mut buf);
        buf
    }

    fn xor(a: &[u8], b: &[u8]) -> Vec<u8> {
        let mut buf = a.to_vec();
        Cipher::xor_in_place(&mut buf, b);
        buf
    }
}
