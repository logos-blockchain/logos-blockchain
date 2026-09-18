use arbitrary_int::u31;
use lb_groth16::{Fr, fr_to_bytes};
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher};

use crate::hd::{ExtendedSecretKey, HardenedIndex, MasterKey, MasterSeed, Mnemonic, ZK_KEY_DST};

// Test vectors of the spec
const MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
// The seed derived from `MNEMONIC` with an empty passphrase
pub(super) const SEED: &str = "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4";
pub(super) const MASTER_KEY: &str =
    "72a5d51d61b1ea9f6ec4bb2287aa26d229443726788bc38440012c44eda58ae8";
pub(super) const MASTER_CHAIN_CODE: &str =
    "c67234c2aebc79aaaa2f79caa66325b86932079331a60f36004658d20d06c3e8";

// Leaves of the key hierarchy, from the spec's test vectors
// m/154'/0'/0'/0'
pub(super) const RECEIVE_0_KEY: &str =
    "dfd5e34a2ffad38e96540489551e0ce5ef10792bd660cf413a9f6dd28a77c016";
pub(super) const RECEIVE_0_CHAIN_CODE: &str =
    "672eeedfdc70109e33ca57b79e1b6fa62f279a162fa203d1ba0163cd70b3d072";
const RECEIVE_0_ZK_KEY: &str = "5e09bf4ce6b3f42970104a6f5940104407f98da0eb946104c13fb4f94c011f16";
// m/154'/0'/0'/1'
pub(super) const RECEIVE_1_KEY: &str =
    "0baeddd9dc5d60ed0dc64597d2b0f49d486e5448cd8ac94fe2515c80fbf5d67c";
pub(super) const RECEIVE_1_CHAIN_CODE: &str =
    "802cdbc751687844cf324e883fa024d80e29d35b4de5d51f151afa5d3c6d2232";
// m/154'/0'/1'/0'
pub(super) const CHANGE_0_KEY: &str =
    "4f3acc80a0fecc95bf8d8cd16435803c5cee29783a6d2809e9fc10840dadb57c";
pub(super) const CHANGE_0_CHAIN_CODE: &str =
    "a85b8265092527323173c6ab624e29faa848dc5cda409a7fbe1d4ec80e8c4130";
// m/154'/0'/2'
pub(super) const VOUCHER_MASTER_KEY: &str =
    "cbbb7fde40e8971d22a7557c890b2d9f00e765bb2475a2588aaba675e6754403";
pub(super) const VOUCHER_MASTER_CHAIN_CODE: &str =
    "775509a7384f889155c37948b8e2677b75975a0f20bd37b0708125d23933aa5d";

#[test]
fn seed_from_mnemonic() {
    let seed = MasterSeed::from_mnemonic(&mnemonic(), "");
    assert_eq!(hex::encode(seed.0), SEED);
}

#[test]
fn passphrase_changes_seed() {
    let seed = MasterSeed::from_mnemonic(&mnemonic(), "TREZOR");
    assert_ne!(hex::encode(seed.0), SEED);
}

#[test]
fn mnemonic_with_invalid_checksum_is_rejected() {
    // The last word `about` carries the checksum, so replacing it breaks
    // the mnemonic even though `abandon` is in the word list.
    let invalid = MNEMONIC.replace("about", "abandon");
    assert!(invalid.parse::<Mnemonic>().is_err());
}

#[test]
fn mnemonic_with_invalid_word_count_is_rejected() {
    assert!("abandon abandon about".parse::<Mnemonic>().is_err());
}

#[test]
fn mnemonic_with_24_words_is_accepted() {
    let mnemonic = format!("{}art", "abandon ".repeat(23));
    assert!(mnemonic.parse::<Mnemonic>().is_ok());
}

#[test]
fn generated_mnemonic_has_12_words() {
    let mnemonic = Mnemonic::generate();
    assert_eq!(mnemonic.to_string().split(' ').count(), 12);
    assert_ne!(mnemonic, Mnemonic::generate());
}

#[test]
fn mnemonic_serde() {
    let json = serde_json::to_string(&mnemonic()).unwrap();
    assert_eq!(json, format!("\"{MNEMONIC}\""));
    assert_eq!(serde_json::from_str::<Mnemonic>(&json).unwrap(), mnemonic());

    assert!(serde_json::from_str::<Mnemonic>("\"abandon abandon about\"").is_err());
}

#[test]
fn master_key_generation() {
    let master = master();
    assert_eq!(hex::encode(master.0.key), MASTER_KEY);
    assert_eq!(hex::encode(master.0.chain_code), MASTER_CHAIN_CODE);
}

#[test]
fn leaf_derivation() {
    let master = master();

    let leaf = master.derive_leaf(&"m/154'/0'/0'/0'".parse().unwrap());
    assert_eq!(hex::encode(leaf.key), RECEIVE_0_KEY);
    assert_eq!(hex::encode(leaf.chain_code), RECEIVE_0_CHAIN_CODE);

    let leaf = master.derive_leaf(&"m/154'/0'/0'/1'".parse().unwrap());
    assert_eq!(hex::encode(leaf.key), RECEIVE_1_KEY);
    assert_eq!(hex::encode(leaf.chain_code), RECEIVE_1_CHAIN_CODE);

    let leaf = master.derive_leaf(&"m/154'/0'/1'/0'".parse().unwrap());
    assert_eq!(hex::encode(leaf.key), CHANGE_0_KEY);
    assert_eq!(hex::encode(leaf.chain_code), CHANGE_0_CHAIN_CODE);

    let leaf = master.derive_leaf(&"m/154'/0'/2'".parse().unwrap());
    assert_eq!(hex::encode(leaf.key), VOUCHER_MASTER_KEY);
    assert_eq!(hex::encode(leaf.chain_code), VOUCHER_MASTER_CHAIN_CODE);
}

#[test]
fn zk_key_derived_from_leaf() {
    let leaf = master().derive_leaf(&"m/154'/0'/0'/0'".parse().unwrap());
    assert_eq!(
        hex::encode(fr_to_bytes(leaf.to_zk_key().as_fr())),
        RECEIVE_0_ZK_KEY
    );
}

#[test]
fn different_indices_derive_different_keys() {
    let key = master().0.clone();
    assert_ne!(
        key.derive_child(index(0)).key,
        key.derive_child(index(1)).key
    );
}

#[test]
fn chain_code_affects_child_key() {
    let key = master().0.clone();
    let other = ExtendedSecretKey {
        key: key.key,
        chain_code: [0u8; 32],
    };
    assert_ne!(
        key.derive_child(index(0)).key,
        other.derive_child(index(0)).key
    );
}

#[test]
fn zk_key_hashes_little_endian_halves_with_dst() {
    let mut key = [0u8; 32];
    key[0] = 1;
    key[16] = 2;
    let extended = ExtendedSecretKey {
        key,
        chain_code: [0u8; 32],
    };
    let expected = Poseidon2Bn254Hasher::digest(&[
        *ZK_KEY_DST,
        // The left half `01 00 .. 00` (little-endian) is 1.
        Fr::from(1u64),
        // The right half `02 00 .. 00` (little-endian) is 2.
        Fr::from(2u64),
    ]);
    assert_eq!(*extended.to_zk_key().as_fr(), expected);
}

#[test]
fn hardened_index_adds_offset() {
    assert_eq!(index(0).0, HardenedIndex::OFFSET);
    assert_eq!(index(HardenedIndex::OFFSET - 1).0, u32::MAX);
    assert_eq!(index(3).child_number(), u31::new(3));
}

#[test]
fn hardened_index_serializes_big_endian() {
    assert_eq!(index(0).to_be_bytes(), [0x80, 0x00, 0x00, 0x00]);
}

pub(super) fn master() -> MasterKey {
    MasterSeed::from_mnemonic(&mnemonic(), "").to_key()
}

fn mnemonic() -> Mnemonic {
    MNEMONIC.parse().unwrap()
}

pub(super) const fn index(number: u32) -> HardenedIndex {
    HardenedIndex::new(u31::new(number))
}
