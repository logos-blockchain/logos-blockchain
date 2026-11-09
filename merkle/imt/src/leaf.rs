use std::sync::LazyLock;

use ark_ff::AdditiveGroup as _;
use lb_groth16::{fr_from_bytes, serde::serde_fr};
use lb_poseidon2::{Digest, Fr};

pub static NULLIFIER_IMT_LEAF_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"NULLIFIER_IMT_LEAF_V1").expect("BigUint should load from constant string")
});

/// The position of a leaf in the tree, in insertion order.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    ::serde::Serialize,
    ::serde::Deserialize,
)]
#[serde(transparent)]
pub struct LeafIndex(u64);

impl LeafIndex {
    #[must_use]
    pub const fn new(index: u64) -> Self {
        Self(index)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl AsRef<u64> for LeafIndex {
    fn as_ref(&self) -> &u64 {
        &self.0
    }
}

/// A leaf of the tree, pointing to the leaf of the next greater nullifier
/// (`next_nullifier` and `next_index` are 0 if there is none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
pub struct NullifierLeaf {
    #[serde(with = "serde_fr")]
    pub nullifier: Fr,
    #[serde(with = "serde_fr")]
    pub next_nullifier: Fr,
    pub next_index: LeafIndex,
}

impl NullifierLeaf {
    /// The first leaf of every tree, below every nullifier.
    pub const SENTINEL: Self = Self {
        nullifier: Fr::ZERO,
        next_nullifier: Fr::ZERO,
        next_index: LeafIndex::new(0),
    };

    #[must_use]
    pub fn hash<Hash: Digest>(&self) -> Fr {
        Hash::digest(&[
            *NULLIFIER_IMT_LEAF_V1,
            self.nullifier,
            self.next_nullifier,
            Fr::from(self.next_index.as_u64()),
        ])
    }
}
