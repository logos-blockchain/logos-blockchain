//! Indexed Merkle tree of nullifiers, proving non membership with the low
//! leaf of a nullifier as defined in the Mantle specification.

use std::{fmt, sync::LazyLock};

use ark_ff::AdditiveGroup as _;
use lb_dynamic_merkle::{DynamicMerkleTree, MerklePath};
use lb_groth16::{fr_from_bytes, serde::serde_fr};
use lb_poseidon2::{Digest, Fr};
use lb_utxotree::UtxoMerkleHasher;
use rpds::{RedBlackTreeMapSync, VectorSync};

static NULLIFIER_IMT_LEAF_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"NULLIFIER_IMT_LEAF_V1").expect("BigUint should load from constant string")
});

/// A leaf of the tree, pointing to the leaf of the next greater nullifier
/// (`next_nullifier` and `next_index` are 0 if there is none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
pub struct NullifierLeaf {
    #[serde(with = "serde_fr")]
    pub nullifier: Fr,
    #[serde(with = "serde_fr")]
    pub next_nullifier: Fr,
    pub next_index: u64,
}

impl NullifierLeaf {
    const SENTINEL: Self = Self {
        nullifier: Fr::ZERO,
        next_nullifier: Fr::ZERO,
        next_index: 0,
    };

    #[must_use]
    pub fn hash<Hash: Digest>(&self) -> Fr {
        Hash::digest(&[
            *NULLIFIER_IMT_LEAF_V1,
            self.nullifier,
            self.next_nullifier,
            Fr::from(self.next_index),
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImtError {
    #[error("nullifier {0} is already in the tree")]
    AlreadyPresent(Fr),
    #[error("the zero nullifier is reserved for the sentinel leaf")]
    ZeroNullifier,
    #[error("the indexed Merkle tree is full")]
    Full,
}

/// A persistent indexed Merkle tree of nullifiers of depth `HEIGHT`.
///
/// The leaves, appended in insertion order, form a linked list sorted by
/// nullifier. The first leaf is the sentinel `(0, 0, 0)` and positions past the
/// last leaf hold 0.
///
/// Only the leaves are serialized, the Merkle tree and the sorted index are
/// rebuilt on deserialization.
pub struct IndexedMerkleTree<Hash: Digest, const HEIGHT: usize> {
    tree: DynamicMerkleTree<UtxoMerkleHasher<Hash>, HEIGHT>,
    leaves: VectorSync<NullifierLeaf>,
    // nullifier to leaf index, the sentinel under 0
    sorted: RedBlackTreeMapSync<Fr, u64>,
}

impl<Hash: Digest, const HEIGHT: usize> Clone for IndexedMerkleTree<Hash, HEIGHT> {
    fn clone(&self) -> Self {
        Self {
            tree: self.tree.clone(),
            leaves: self.leaves.clone(),
            sorted: self.sorted.clone(),
        }
    }
}

impl<Hash: Digest, const HEIGHT: usize> fmt::Debug for IndexedMerkleTree<Hash, HEIGHT> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndexedMerkleTree")
            .field("leaves", &self.leaves)
            .finish_non_exhaustive()
    }
}

impl<Hash: Digest, const HEIGHT: usize> PartialEq for IndexedMerkleTree<Hash, HEIGHT> {
    fn eq(&self, other: &Self) -> bool {
        self.leaves == other.leaves
    }
}

impl<Hash: Digest, const HEIGHT: usize> Eq for IndexedMerkleTree<Hash, HEIGHT> {}

impl<Hash: Digest, const HEIGHT: usize> Default for IndexedMerkleTree<Hash, HEIGHT> {
    fn default() -> Self {
        Self::new()
    }
}

#[expect(
    clippy::len_without_is_empty,
    reason = "the sentinel leaf makes the tree never empty"
)]
impl<Hash: Digest, const HEIGHT: usize> IndexedMerkleTree<Hash, HEIGHT> {
    #[must_use]
    pub fn new() -> Self {
        let sentinel = NullifierLeaf::SENTINEL;
        Self {
            tree: DynamicMerkleTree::new().insert(sentinel.hash::<Hash>()).0,
            leaves: VectorSync::new_sync().push_back(sentinel),
            sorted: RedBlackTreeMapSync::new_sync().insert(Fr::ZERO, 0),
        }
    }

    #[must_use]
    pub fn contains(&self, nullifier: &Fr) -> bool {
        self.sorted.contains_key(nullifier)
    }

    pub fn insert(&self, nullifier: Fr) -> Result<Self, ImtError> {
        if nullifier == Fr::ZERO {
            return Err(ImtError::ZeroNullifier);
        }
        let (low_index, low) = self
            .low_leaf(&nullifier)
            .ok_or(ImtError::AlreadyPresent(nullifier))?;
        let n = self.leaves.len();
        if n == 1 << HEIGHT {
            return Err(ImtError::Full);
        }

        let new_leaf = NullifierLeaf {
            nullifier,
            next_nullifier: low.next_nullifier,
            next_index: low.next_index,
        };
        let low = NullifierLeaf {
            next_nullifier: nullifier,
            next_index: n as u64,
            ..*low
        };
        let tree = self.tree.update(low_index as usize, low.hash::<Hash>());
        let (tree, index) = tree.insert(new_leaf.hash::<Hash>());
        assert_eq!(index, n, "leaves are appended in insertion order");

        Ok(Self {
            tree,
            leaves: self
                .leaves
                .set(low_index as usize, low)
                .expect("low leaf is in the tree")
                .push_back(new_leaf),
            sorted: self.sorted.insert(nullifier, n as u64),
        })
    }

    #[must_use]
    pub fn root(&self) -> Fr {
        self.tree.root()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    #[must_use]
    pub fn leaf(&self, index: u64) -> Option<&NullifierLeaf> {
        self.leaves.get(usize::try_from(index).ok()?)
    }

    /// Returns the leaf of the greatest nullifier lower than `nullifier`, or
    /// `None` if `nullifier` is in the tree.
    #[must_use]
    pub fn low_leaf(&self, nullifier: &Fr) -> Option<(u64, &NullifierLeaf)> {
        if self.contains(nullifier) {
            return None;
        }
        let (_, index) = self.sorted.range(..*nullifier).next_back()?;
        Some((*index, self.leaf(*index)?))
    }

    #[must_use]
    pub fn path(&self, index: u64) -> Option<MerklePath<Fr, HEIGHT>> {
        self.tree.path(usize::try_from(index).ok()?)
    }

    fn from_leaves(leaves: VectorSync<NullifierLeaf>) -> Result<Self, &'static str> {
        if leaves.len() > 1 << HEIGHT {
            return Err("more leaves than the tree capacity");
        }
        if leaves.first().map(|sentinel| sentinel.nullifier) != Some(Fr::ZERO) {
            return Err("first leaf is not the sentinel");
        }
        let mut sorted = RedBlackTreeMapSync::new_sync();
        for (index, leaf) in leaves.iter().enumerate() {
            if sorted.contains_key(&leaf.nullifier) {
                return Err("duplicate nullifier");
            }
            sorted.insert_mut(leaf.nullifier, index as u64);
        }
        let mut entries = sorted.iter().peekable();
        while let Some((_, index)) = entries.next() {
            let (next_nullifier, next_index) = entries
                .peek()
                .map_or((Fr::ZERO, 0), |(next_nullifier, next_index)| {
                    (**next_nullifier, **next_index)
                });
            let leaf = &leaves[*index as usize];
            if leaf.next_nullifier != next_nullifier || leaf.next_index != next_index {
                return Err("leaves do not form the sorted linked list");
            }
        }
        let tree = DynamicMerkleTree::from_sorted_items(
            leaves
                .iter()
                .enumerate()
                .map(|(index, leaf)| (index, leaf.hash::<Hash>())),
        );
        Ok(Self {
            tree,
            leaves,
            sorted,
        })
    }
}

mod serde {
    use lb_poseidon2::Digest;
    use rpds::VectorSync;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct as _};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        leaves: VectorSync<super::NullifierLeaf>,
    }

    impl<Hash: Digest, const HEIGHT: usize> Serialize for super::IndexedMerkleTree<Hash, HEIGHT> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut state = serializer.serialize_struct("IndexedMerkleTree", 1)?;
            state.serialize_field("leaves", &self.leaves)?;
            state.end()
        }
    }

    impl<'de, Hash: Digest, const HEIGHT: usize> Deserialize<'de>
        for super::IndexedMerkleTree<Hash, HEIGHT>
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let raw = Raw::deserialize(deserializer)?;
            Self::from_leaves(raw.leaves).map_err(serde::de::Error::custom)
        }
    }
}

#[cfg(test)]
mod tests {
    use ark_ff::Field as _;
    use lb_dynamic_merkle::{MerkleHasher as _, MerkleNode};
    use num_bigint::BigUint;
    use proptest_macro::property_test;

    use super::*;

    type ZkHasher = lb_poseidon2::Poseidon2Bn254Hasher;
    type TestHasher = UtxoMerkleHasher<ZkHasher>;
    type Imt = IndexedMerkleTree<ZkHasher, 40>;

    fn fr(n: u64) -> Fr {
        Fr::from(n)
    }

    fn leaf(nullifier: u64, next_nullifier: u64, next_index: u64) -> NullifierLeaf {
        NullifierLeaf {
            nullifier: fr(nullifier),
            next_nullifier: fr(next_nullifier),
            next_index,
        }
    }

    // Depth 40 root of the given leaf hashes, every later position holding 0.
    fn naive_root(leaves: &[Fr]) -> Fr {
        let mut nodes = leaves.to_vec();
        let mut empty = Fr::ZERO;
        for _ in 0..40 {
            if nodes.len() % 2 == 1 {
                nodes.push(empty);
            }
            nodes = nodes
                .chunks(2)
                .map(|pair| ZkHasher::compress(&[pair[0], pair[1]]))
                .collect();
            empty = ZkHasher::compress(&[empty, empty]);
        }
        nodes.first().copied().unwrap_or(empty)
    }

    fn imt_leaf_hashes(imt: &Imt) -> Vec<Fr> {
        (0..imt.len() as u64)
            .map(|index| imt.leaf(index).unwrap().hash::<ZkHasher>())
            .collect()
    }

    // Nullifiers in linked list order, starting after the sentinel.
    fn walk_sorted(imt: &Imt) -> Vec<Fr> {
        let mut nullifiers = Vec::new();
        let mut current = imt.leaf(0).unwrap();
        while current.next_nullifier != Fr::ZERO {
            current = imt.leaf(current.next_index).unwrap();
            nullifiers.push(current.nullifier);
        }
        nullifiers
    }

    fn path_root(leaf: Fr, path: &MerklePath<Fr, 40>) -> Fr {
        path.iter().fold(leaf, |node, sibling| match sibling {
            MerkleNode::Left(left) => ZkHasher::compress(&[*left, node]),
            MerkleNode::Right(right) => ZkHasher::compress(&[node, *right]),
        })
    }

    #[test]
    fn test_leaf_dst_matches_circuits() {
        let expected: BigUint = "72106678126369435953119723346337253106302222882126"
            .parse()
            .unwrap();
        assert_eq!(*NULLIFIER_IMT_LEAF_V1, Fr::from(expected));
    }

    #[test]
    fn test_empty_root_is_sentinel_only() {
        let imt = Imt::new();
        let sentinel = NullifierLeaf {
            nullifier: Fr::ZERO,
            next_nullifier: Fr::ZERO,
            next_index: 0,
        };
        assert_eq!(imt.root(), naive_root(&[sentinel.hash::<ZkHasher>()]));
        assert_ne!(imt.root(), TestHasher::empty_subtree_root(40)); // the sentinel is not an empty position
    }

    #[test]
    fn test_insert_links_sorted_list() {
        let imt = Imt::new()
            .insert(fr(30))
            .unwrap()
            .insert(fr(10))
            .unwrap()
            .insert(fr(20))
            .unwrap();
        assert_eq!(*imt.leaf(0).unwrap(), leaf(0, 10, 2));
        assert_eq!(*imt.leaf(1).unwrap(), leaf(30, 0, 0));
        assert_eq!(*imt.leaf(2).unwrap(), leaf(10, 20, 3));
        assert_eq!(*imt.leaf(3).unwrap(), leaf(20, 30, 1));
        assert_eq!(imt.root(), naive_root(&imt_leaf_hashes(&imt)));
    }

    #[test]
    fn test_low_leaf_proves_non_membership() {
        let imt = Imt::new().insert(fr(10)).unwrap().insert(fr(30)).unwrap();
        let (index, low) = imt.low_leaf(&fr(20)).unwrap();
        assert_eq!(
            (index, low.nullifier, low.next_nullifier),
            (1, fr(10), fr(30))
        );
        assert!(imt.low_leaf(&fr(30)).is_none());
        let (_, top) = imt.low_leaf(&fr(99)).unwrap();
        assert_eq!((top.nullifier, top.next_nullifier), (fr(30), Fr::ZERO));
    }

    #[test]
    fn test_order_is_integer_order_near_modulus() {
        let big = -Fr::ONE; // p - 1
        let mid = Fr::from(BigUint::from(1u8) << 200);
        let imt = Imt::new()
            .insert(big)
            .unwrap()
            .insert(mid)
            .unwrap()
            .insert(fr(5))
            .unwrap();
        assert_eq!(walk_sorted(&imt), vec![fr(5), mid, big]);
    }

    #[test]
    fn test_rejects_duplicate_and_zero_without_change() {
        let imt = Imt::new().insert(fr(10)).unwrap();
        assert_eq!(imt.insert(fr(10)), Err(ImtError::AlreadyPresent(fr(10))));
        assert_eq!(imt.insert(Fr::ZERO), Err(ImtError::ZeroNullifier));
        assert_eq!(imt.len(), 2);
    }

    #[test]
    fn test_serde_roundtrip_rebuilds_tree() {
        let imt = Imt::new().insert(fr(30)).unwrap().insert(fr(10)).unwrap();
        let serialized = serde_json::to_string(&imt).unwrap();
        let recovered: Imt = serde_json::from_str(&serialized).unwrap();
        assert_eq!(recovered, imt);
        assert_eq!(recovered.root(), imt.root());
        assert_eq!(recovered.low_leaf(&fr(20)), imt.low_leaf(&fr(20)));
    }

    #[property_test]
    fn test_path_verifies_every_leaf(nullifiers: Vec<u64>) {
        let mut imt = Imt::new();
        for nullifier in nullifiers.into_iter().filter(|nullifier| *nullifier != 0) {
            if !imt.contains(&fr(nullifier)) {
                imt = imt.insert(fr(nullifier)).unwrap();
            }
        }
        for (index, hash) in imt_leaf_hashes(&imt).into_iter().enumerate() {
            let path = imt.path(index as u64).unwrap();
            assert_eq!(path_root(hash, &path), imt.root());
        }
    }
}
