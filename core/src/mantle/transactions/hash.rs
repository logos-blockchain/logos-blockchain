use ark_ff::PrimeField as _;
use bytes::Bytes;
use lb_codec::BinaryCodec;
use lb_groth16::Fr;

use crate::{
    crypto::Hash,
    utils::{display_hex_bytes_newtype, serde_bytes_newtype},
};

/// The hash of a transaction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord, BinaryCodec)]
pub struct TxHash(pub Hash);
serde_bytes_newtype!(TxHash, 32);

/// Number of leading hash bytes a block proposal uses to refer to a
/// transaction.
pub const REFERENCE_PREFIX_BYTES: usize = 16;

/// The leading [`REFERENCE_PREFIX_BYTES`] bytes of a [`TxHash`] — how a block
/// proposal refers to a transaction that is already in a validator's mempool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord, BinaryCodec)]
pub struct TxHashPrefix(pub [u8; REFERENCE_PREFIX_BYTES]);
serde_bytes_newtype!(TxHashPrefix, REFERENCE_PREFIX_BYTES);
display_hex_bytes_newtype!(TxHashPrefix);

impl From<[u8; REFERENCE_PREFIX_BYTES]> for TxHashPrefix {
    fn from(prefix: [u8; REFERENCE_PREFIX_BYTES]) -> Self {
        Self(prefix)
    }
}

/// A mempool key that a block proposal can refer to by a short prefix.
pub trait PrefixedKey {
    /// The short form a proposal carries in place of the full key.
    type Prefix;

    /// The prefix a proposal would use to refer to this key.
    fn key_prefix(&self) -> Self::Prefix;
}

impl PrefixedKey for TxHash {
    type Prefix = TxHashPrefix;

    fn key_prefix(&self) -> Self::Prefix {
        self.prefix()
    }
}

impl From<Hash> for TxHash {
    fn from(hash: Hash) -> Self {
        Self(hash)
    }
}

impl From<TxHash> for Hash {
    fn from(hash: TxHash) -> Self {
        hash.0
    }
}

impl AsRef<Hash> for TxHash {
    fn as_ref(&self) -> &Hash {
        &self.0
    }
}

impl From<TxHash> for Bytes {
    fn from(tx_hash: TxHash) -> Self {
        Self::copy_from_slice(&tx_hash.0)
    }
}

impl TxHash {
    /// For testing purposes
    #[cfg(test)]
    pub fn random(mut rng: impl rand::RngCore) -> Self {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// The short form a block proposal uses to refer to this transaction.
    // TODO: Done this way so we can keep the function a `const`.
    #[must_use]
    pub const fn prefix(&self) -> TxHashPrefix {
        let mut prefix = [0u8; REFERENCE_PREFIX_BYTES];
        let mut index = 0;
        while index < REFERENCE_PREFIX_BYTES {
            prefix[index] = self.0[index];
            index = index.checked_add(1).unwrap();
        }
        TxHashPrefix(prefix)
    }

    #[must_use]
    pub fn as_signing_bytes(&self) -> Bytes {
        Bytes::from(self.0.to_vec())
    }

    #[must_use]
    pub fn to_fr(&self) -> Fr {
        Fr::from_le_bytes_mod_order(&self.0)
    }
}

/// Holds a reference to a [`TxHash`] and its corresponding [`Bytes`] and [`Fr`]
/// representations, to avoid repeated conversions.
pub struct TxHashView {
    tx_hash: TxHash,
    tx_hash_bytes: Bytes,
    tx_hash_fr: Fr,
}

impl TxHashView {
    #[must_use]
    pub fn new(tx_hash: TxHash) -> Self {
        let tx_hash_bytes = tx_hash.as_signing_bytes();
        let tx_hash_fr = tx_hash.to_fr();
        Self {
            tx_hash,
            tx_hash_bytes,
            tx_hash_fr,
        }
    }

    #[must_use]
    pub const fn tx_hash(&self) -> &TxHash {
        &self.tx_hash
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &Bytes {
        &self.tx_hash_bytes
    }

    #[must_use]
    pub const fn as_fr(&self) -> &Fr {
        &self.tx_hash_fr
    }
}

impl From<TxHash> for TxHashView {
    fn from(value: TxHash) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use crate::mantle::{
        TxHash,
        transactions::hash::{PrefixedKey as _, REFERENCE_PREFIX_BYTES, TxHashPrefix},
    };

    #[test]
    fn prefix_is_the_leading_hash_bytes() {
        let mut hash = [0u8; 32];
        for (index, byte) in hash.iter_mut().enumerate() {
            *byte = u8::try_from(index).unwrap();
        }

        assert_eq!(
            TxHash(hash).prefix(),
            TxHashPrefix([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
        );
    }

    #[test]
    fn hashes_sharing_a_prefix_collide_while_differing_beyond_it() {
        let mut first = [0xAAu8; 32];
        let mut second = [0xAAu8; 32];
        // Differ only past the prefix.
        first[REFERENCE_PREFIX_BYTES] = 0x01;
        second[REFERENCE_PREFIX_BYTES] = 0x02;

        assert_ne!(TxHash(first), TxHash(second));
        assert_eq!(TxHash(first).prefix(), TxHash(second).prefix());
    }

    #[test]
    fn prefix_matches_the_prefixed_key_impl() {
        let hash = TxHash([0x5Au8; 32]);
        assert_eq!(hash.prefix(), hash.key_prefix());
    }
}
