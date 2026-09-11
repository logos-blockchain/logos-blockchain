//! Hierarchical deterministic key derivation
//!
//! Spec: [Wallet Technical Standard](https://lip.logos.co/blockchain/raw/wallet-technical-standard.html)

use std::sync::LazyLock;

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

const MASTER_KEY_PERSONALIZATION: &[u8; 16] = b"Logos_MasterKGen";
const CHILD_KEY_PERSONALIZATION: &[u8; 16] = b"Logos_ExpandSeed";
static ZK_KEY_DST: LazyLock<Fr> = LazyLock::new(|| fr_from_bytes_unchecked(b"WALLET_ZK_SK_V1"));

/// A secret key with a chain code, from which hardened child keys are derived.
#[derive(Clone, ZeroizeOnDrop)]
pub struct ExtendedSecretKey {
    key: [u8; 32],
    chain_code: [u8; 32],
}

impl ExtendedSecretKey {
    /// Derives the master key from a seed.
    #[must_use]
    pub fn from_seed(seed: &[u8]) -> Self {
        Self::from_hash(&blake2b512(MASTER_KEY_PERSONALIZATION, &[seed]))
    }

    /// Derives the hardened child key at `index`.
    #[must_use]
    pub fn derive_child(&self, index: HardenedIndex) -> Self {
        Self::from_hash(&blake2b512(
            CHILD_KEY_PERSONALIZATION,
            &[&self.chain_code, &[0x00], &self.key, &index.to_be_bytes()],
        ))
    }

    fn from_hash(hash: &[u8; 64]) -> Self {
        let (key, chain_code) = hash.split_at(32);
        Self {
            key: key.try_into().expect("Hash half is 32 bytes"),
            chain_code: chain_code.try_into().expect("Hash half is 32 bytes"),
        }
    }

    /// Derives the key at `path`, one hardened child per level.
    #[must_use]
    pub fn derive_path(&self, path: &[HardenedIndex]) -> Self {
        path.iter()
            .fold(self.clone(), |key, index| key.derive_child(*index))
    }

    /// Converts this key into the [`ZkKey`] used in logos-blockchain.
    #[must_use]
    pub fn to_zk_key(&self) -> ZkKey {
        let (left, right) = self.key.split_at(16);
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
    pub const fn new(child_number: u32) -> Result<Self, HardenedIndexError> {
        if child_number >= Self::OFFSET {
            return Err(HardenedIndexError::ChildNumberTooLarge(child_number));
        }
        Ok(Self(child_number + Self::OFFSET))
    }

    const fn to_be_bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HardenedIndexError {
    #[error("child number {0} must be smaller than 2^31")]
    ChildNumberTooLarge(u32),
}

/// Unkeyed BLAKE2b-512 with a 16-byte personalization string.
fn blake2b512(personalization: &[u8; 16], inputs: &[&[u8]]) -> Zeroizing<[u8; 64]> {
    let mut core = Blake2bVarCore::new_with_params(&[], personalization, 0, 64);
    let mut buffer = Buffer::<Blake2bVarCore>::default();
    for input in inputs {
        buffer.digest_blocks(input, |blocks| core.update_blocks(blocks));
    }
    let mut output = Output::<Blake2bVarCore>::default();
    core.finalize_variable_core(&mut buffer, &mut output);
    Zeroizing::new(output.into())
}
