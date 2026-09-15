//! Hierarchical deterministic key derivation
//!
//! Spec: [Wallet Technical Standard](https://lip.logos.co/blockchain/raw/wallet-technical-standard.html)

use std::sync::LazyLock;

pub use arbitrary_int::u31;
use blake2::{
    Blake2bVarCore,
    digest::{
        Output,
        core_api::{Buffer, UpdateCore as _, VariableOutputCore as _},
    },
};
use lb_groth16::{Fr, fr_from_bytes_unchecked};
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher};
use zeroize::{ZeroizeOnDrop, Zeroizing};

use crate::keys::ZkKey;

#[cfg(test)]
mod tests;

const BLAKE2B_PERSONA_SIZE: usize = 16;
const MASTER_KEY_PERSONALIZATION: &[u8; BLAKE2B_PERSONA_SIZE] = b"Logos_MasterKGen";
const CHILD_KEY_PERSONALIZATION: &[u8; BLAKE2B_PERSONA_SIZE] = b"Logos_ExpandSeed";
static ZK_KEY_DST: LazyLock<Fr> = LazyLock::new(|| fr_from_bytes_unchecked(b"WALLET_ZK_SK_V1"));
const HASH_SIZE: usize = 64;
const HALF_HASH_SIZE: usize = div_exact(HASH_SIZE, 2);

/// A 64-byte master seed from which the master key is derived.
#[derive(ZeroizeOnDrop)]
pub struct MasterSeed([u8; 64]);

/// A secret key with a chain code, from which hardened child keys are derived.
#[derive(Clone, ZeroizeOnDrop)]
pub struct ExtendedSecretKey {
    key: [u8; HALF_HASH_SIZE],
    chain_code: [u8; HALF_HASH_SIZE],
}

impl ExtendedSecretKey {
    /// Derives the master key from a seed.
    #[must_use]
    pub fn from_seed(seed: &MasterSeed) -> Self {
        Self::from_hash(&blake2b512(MASTER_KEY_PERSONALIZATION, &[&seed.0]))
    }

    /// Derives the hardened child key at `index`.
    #[must_use]
    pub fn derive_child(&self, index: HardenedIndex) -> Self {
        Self::from_hash(&blake2b512(
            CHILD_KEY_PERSONALIZATION,
            &[&self.chain_code, &[0x00], &self.key, &index.to_be_bytes()],
        ))
    }

    fn from_hash(hash: &[u8; HASH_SIZE]) -> Self {
        let (key, chain_code) = hash.split_at(HALF_HASH_SIZE);
        Self {
            key: key.try_into().expect("Hash half is HALF_HASH_SIZE bytes"),
            chain_code: chain_code
                .try_into()
                .expect("Hash half is HALF_HASH_SIZE bytes"),
        }
    }

    /// Derives the key at `path`, one hardened child per level.
    #[must_use]
    pub fn derive_path(&self, path: &Path) -> Self {
        path.iter()
            .fold(self.clone(), |key, index| key.derive_child(*index))
    }

    /// Converts this key into the [`ZkKey`] used in logos-blockchain.
    #[must_use]
    pub fn to_zk_key(&self) -> ZkKey {
        let (left, right) = self.key.split_at(div_exact(self.key.len(), 2));
        ZkKey::new(Poseidon2Bn254Hasher::digest(&[
            *ZK_KEY_DST,
            fr_from_bytes_unchecked(left),
            fr_from_bytes_unchecked(right),
        ]))
    }

    /// Convert this key into a STARK key used for PQ (post-quantum)
    pub fn to_stark_key(&self) {
        todo!("derive/return a STARK key");
    }
}

/// HD path, a sequence of hardened child indices.
pub type Path = [HardenedIndex];

/// The index of a hardened child key, in the range `[2^31, 2^32)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardenedIndex(u32);

impl HardenedIndex {
    /// The smallest hardened index
    const OFFSET: u32 = 1 << 31;

    /// Converts `child_number` to the hardened index in the range `[2^31,
    /// 2^32)`, as specified in the spec and BIP-32.
    ///
    /// `child_number` must be in the range `[0, 2^31)`.
    ///
    /// Thanks to this conversion, callers can write HD paths with the small
    /// `child_number` instead of the large index.
    #[must_use]
    pub const fn new(child_number: u31) -> Self {
        Self(child_number.value() + Self::OFFSET)
    }

    const fn to_be_bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

/// Unkeyed BLAKE2b-512 with a 16-byte personalization string.
fn blake2b512(
    personalization: &[u8; BLAKE2B_PERSONA_SIZE],
    inputs: &[&[u8]],
) -> Zeroizing<[u8; HASH_SIZE]> {
    let mut core = Blake2bVarCore::new_with_params(&[], personalization, 0, HASH_SIZE);
    let mut buffer = Buffer::<Blake2bVarCore>::default();
    for input in inputs {
        buffer.digest_blocks(input, |blocks| core.update_blocks(blocks));
    }
    let mut output = Output::<Blake2bVarCore>::default();
    core.finalize_variable_core(&mut buffer, &mut output);
    Zeroizing::new(output.into())
}

/// Divides `a` by `b`, asserting that the division is exact.
///
/// Replace with `a.div_exact(b)` once `usize::div_exact` is stabilized.
const fn div_exact(a: usize, b: usize) -> usize {
    assert!(a.is_multiple_of(b));
    a / b
}
