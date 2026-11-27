//! Costs of note encryption for a sender and for a wallet.
//!
//! A sender pays `encrypt` once per note. A wallet pays `decrypt` once per
//! ciphertext it tries and per key it holds, whether the note is its own or
//! not, and `keys_of_secret_key` once per key and batch of ciphertexts.
//!
//! Run with
//!
//! ```text
//! cargo bench -p logos-blockchain-note-encryption --bench note_encryption [-- <filter>]
//! ```

use divan::{Bencher, black_box};
use logos_blockchain_note_encryption::{
    DecapsulationKey, EncapsulationKey, decrypt_note, encrypt_note,
};
use rand::{SeedableRng as _, rngs::StdRng};

const K_LOGOS: [u8; 32] = [7; 32];
const COMMITMENT: [u8; 32] = [1; 32];
const VALUE: u64 = 1_000_000;
const NONCE: [u8; 32] = [2; 32];

fn main() {
    divan::main();
}

fn rng() -> StdRng {
    StdRng::seed_from_u64(0)
}

/// Deriving `sk_enc` from `k_logos` and expanding its keys.
#[divan::bench]
fn keys_of_secret_key() -> DecapsulationKey {
    DecapsulationKey::from_secret_key(black_box(&K_LOGOS))
}

/// Parsing the encryption key of an address, with the modulus check.
#[divan::bench]
fn parse_encryption_key(bencher: Bencher) {
    let bytes = DecapsulationKey::from_secret_key(&K_LOGOS)
        .encapsulation_key()
        .to_bytes();
    bencher.bench(|| EncapsulationKey::from_bytes(black_box(&bytes)));
}

#[divan::bench]
fn encrypt(bencher: Bencher) {
    let pk_enc = DecapsulationKey::from_secret_key(&K_LOGOS).encapsulation_key();
    let mut rng = rng();
    bencher.bench_local(|| encrypt_note(&pk_enc, &COMMITMENT, VALUE, &NONCE, &mut rng));
}

/// Opening a note of the wallet.
#[divan::bench]
fn decrypt(bencher: Bencher) {
    let sk_enc = DecapsulationKey::from_secret_key(&K_LOGOS);
    let ciphertext = encrypt_note(
        &sk_enc.encapsulation_key(),
        &COMMITMENT,
        VALUE,
        &NONCE,
        &mut rng(),
    );
    bencher.bench(|| decrypt_note(&sk_enc, &COMMITMENT, black_box(&ciphertext)));
}

/// Trying a note of another wallet, which fails.
#[divan::bench]
fn decrypt_other_wallet(bencher: Bencher) {
    let sender_target = DecapsulationKey::from_secret_key(&K_LOGOS);
    let sk_enc = DecapsulationKey::from_secret_key(&[8; 32]);
    let ciphertext = encrypt_note(
        &sender_target.encapsulation_key(),
        &COMMITMENT,
        VALUE,
        &NONCE,
        &mut rng(),
    );
    bencher.bench(|| decrypt_note(&sk_enc, &COMMITMENT, black_box(&ciphertext)));
}
