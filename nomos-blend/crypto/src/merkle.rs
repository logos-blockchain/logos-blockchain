use core::fmt::{self, Debug, Formatter};
use std::collections::HashMap;

use groth16::{fr_from_bytes_unchecked, fr_to_bytes};
use poq::{CORE_MERKLE_TREE_HEIGHT, CorePathAndSelectors};
use rs_merkle_tree::{Node, stores::MemoryStore, tree::MerkleProof};
use thiserror::Error;

use crate::{ZkHash, ZkHasher};

type ZkPublicKey = ZkHash;
const TOTAL_MERKLE_LEAVES: usize = 1 << CORE_MERKLE_TREE_HEIGHT;

#[derive(Debug, Error, PartialEq, Eq, PartialOrd, Ord)]
pub enum Error {
    #[error("Cannot create Merkle tree with zero keys.")]
    EmptyKeySet,
    #[error("Cannot create Merkle tree with more than {TOTAL_MERKLE_LEAVES} keys.")]
    TooManyKeys,
    #[error("Cannot create Merkle tree with duplicate items.")]
    DuplicateKey,
    #[cfg(test)]
    #[error("Provided key for proof verification is not part of the underlying Merkle tree.")]
    KeyNotFound,
    #[cfg(test)]
    #[error("Invalid proof.")]
    InvalidProof,
}

struct InnerTreeZkHasher;

impl rs_merkle_tree::hasher::Hasher for InnerTreeZkHasher {
    fn hash(&self, left: &Node, right: &Node) -> Node {
        let mut hasher = ZkHasher::new();
        hasher.compress(&[
            // We use `unchecked` because we control the inputs, and poseidon hasher is guaranteed
            // to always output valid `Fr` points.
            fr_from_bytes_unchecked(left.as_ref()),
            fr_from_bytes_unchecked(right.as_ref()),
        ]);
        fr_to_bytes(&hasher.finalize()).into()
    }
}

/// A membership-specific Merkle tree that indices information about core nodes'
/// ZK keys.
///
/// It is a fixed-height tree, with the height expected by the [`PoQ` specification](https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#215261aa09df81ec850ad7965bf6e76b).
/// It is a wrapped around an instance of an [`rs_merkle_tree`], configured with
/// our [`nomos_core::crypto::ZkHasher`] and additional information to make it
/// suitable for `PoQ` usage.
pub struct MerkleTree {
    /// A map of key -> index after the input keys have been sorted, for proof
    /// generation starting from a given key.
    sorted_key_indices: HashMap<ZkPublicKey, usize>,
    /// The inner [`rs_merkle_tree::MerkleTree`] instance.
    inner_tree: rs_merkle_tree::MerkleTree<InnerTreeZkHasher, MemoryStore, CORE_MERKLE_TREE_HEIGHT>,
}

impl PartialEq for MerkleTree {
    fn eq(&self, other: &Self) -> bool {
        self.root() == other.root()
    }
}

impl Debug for MerkleTree {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("MerkleTree")
            .field("root", &self.root())
            .finish()
    }
}

impl MerkleTree {
    /// Create a new merkle tree with the provided keys.
    ///
    /// Keys are internally sorted by their numeric value, as described in the [`PoQ` specification](https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#215261aa09df81ec850ad7965bf6e76b).
    pub fn new(mut keys: Vec<ZkPublicKey>) -> Result<Self, Error> {
        // Sort the input keys by their decimal representation, relying on `Fr`'s
        // implementation of `PartialOrd`.
        keys.sort();
        Self::new_from_ordered(keys)
    }

    /// Create a new merkle tree with the provided, already-sorted keys, as described in the [`PoQ` specification](https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#215261aa09df81ec850ad7965bf6e76b).
    ///
    /// If the input vector is empty or if it is larger than the maximum number
    /// of leaves supported by this fixed-height Merkle tree, it returns an
    /// error.
    fn new_from_ordered(keys: Vec<ZkPublicKey>) -> Result<Self, Error> {
        if keys.is_empty() {
            return Err(Error::EmptyKeySet);
        }
        if keys.len() > TOTAL_MERKLE_LEAVES {
            return Err(Error::TooManyKeys);
        }

        let sorted_key_indices = keys
            .iter()
            .enumerate()
            .map(|(index, key)| (*key, index))
            .collect::<HashMap<_, _>>();
        // We do not accept duplicate keys as they would cause issues with proof
        // generation.
        if sorted_key_indices.len() != keys.len() {
            return Err(Error::DuplicateKey);
        }

        let inner_merkle_tree = {
            let mut inner_tree =
                rs_merkle_tree::MerkleTree::new(InnerTreeZkHasher, MemoryStore::new());
            inner_tree
                .add_leaves(
                    &keys
                        .into_iter()
                        .map(|key| fr_to_bytes(&key).into())
                        .collect::<Vec<_>>(),
                )
                .expect("Adding leaves should not fail because we already check for input length and duplicates.");
            inner_tree
        };

        Ok(Self {
            inner_tree: inner_merkle_tree,
            sorted_key_indices,
        })
    }

    /// Returns the Merkle root resulting from the input keys.
    ///
    /// Being a fixed-height tree, the Merkle root is computed padding the
    /// missing leaves with the hashes of the empty sub-trees for each level.
    #[must_use]
    pub fn root(&self) -> ZkHash {
        fr_from_bytes_unchecked(
            self.inner_tree
                .root()
                .expect("Inner Merkle tree should have a root.")
                .as_ref(),
        )
    }

    /// Construct a merkle proof for a given key, if present in the original
    /// input.
    ///
    /// The path is made of tuples of `(hash, boolean)`, where the first is the
    /// hash of the node in the path, akin to every Merkle proof, and the second
    /// is a selector that is `false` if the node is a left child of its parent,
    /// and `true` otherwise.
    ///
    /// The resulting path has the same length as the fixed height of the tree.
    #[must_use]
    pub fn get_proof_for_key(&self, key: &ZkPublicKey) -> Option<CorePathAndSelectors> {
        let key_index = self.sorted_key_indices.get(key).copied()?;

        let proof = self
            .inner_tree
            .proof(key_index as u64)
            .expect("Merkle proof should generated successfully for the given key.");
        // Because the inner Merkle tree implementation only gives us a path, we
        // re-compute the selectors ourselves.
        let selectors_for_proof = compute_selectors(&proof);

        Some(
            proof
                .proof
                .iter()
                .zip(selectors_for_proof)
                .map(|(node, selector)| (fr_from_bytes_unchecked(node.as_ref()), selector))
                .collect::<Vec<_>>()
                .try_into()
                .expect("Should not fail to map proof hashes and selectors into a single array, since they are of the same and required length."),
        )
    }

    #[cfg(test)]
    fn verify_proof_for_key(
        &self,
        proof: &CorePathAndSelectors,
        key: &ZkPublicKey,
    ) -> Result<(), Error> {
        let Some(key_index) = self.sorted_key_indices.get(key) else {
            return Err(Error::KeyNotFound);
        };
        let inner_proof = MerkleProof {
            proof: proof.map(|(hash, _)| fr_to_bytes(&hash).into()),
            index: *key_index as u64,
            leaf: fr_to_bytes(key).into(),
            root: fr_to_bytes(&self.root()).into(),
        };
        let Ok(true) = self.inner_tree.verify_proof(&inner_proof) else {
            return Err(Error::InvalidProof);
        };
        Ok(())
    }
}

// Compute the path selectors of a path from the given leaf to the the given
// root.
fn compute_selectors(
    MerkleProof { index, .. }: &MerkleProof<CORE_MERKLE_TREE_HEIGHT>,
) -> [bool; CORE_MERKLE_TREE_HEIGHT] {
    let mut result = [false; CORE_MERKLE_TREE_HEIGHT];
    let mut idx = *index;

    // The selector at each level is determined by the corresponding bit of the leaf
    // index. Iterating from the last element to the first (leaf → root):
    // result[CORE_MERKLE_TREE_HEIGHT-1] (leaf level) = bit 0 (LSB) of index
    // result[0] (root level) = MSB of index
    for result_entry in result.iter_mut().take(CORE_MERKLE_TREE_HEIGHT).rev() {
        *result_entry = (idx & 1) == 1;
        idx >>= 1u8;
    }

    result
}

pub fn sort_nodes_and_build_merkle_tree<Node>(
    nodes: &mut [Node],
    key: impl Fn(&Node) -> ZkPublicKey,
) -> Result<MerkleTree, Error> {
    nodes.sort_by_key(|node| key(node));
    MerkleTree::new_from_ordered(nodes.iter().map(key).collect())
}

#[cfg(test)]
mod tests {
    use core::iter::repeat_n;

    use groth16::{Field as _, fr_from_bytes_unchecked};
    use num_bigint::BigUint;

    use crate::{
        ZkHash,
        merkle::{Error, MerkleTree, TOTAL_MERKLE_LEAVES},
    };

    #[test]
    fn single_key() {
        let input_key = ZkHash::ONE;

        let merkle_tree = MerkleTree::new(vec![input_key]).unwrap();

        let merkle_root = merkle_tree.root();
        assert_ne!(input_key, merkle_root);

        let proof = merkle_tree.get_proof_for_key(&input_key).unwrap();
        merkle_tree
            .verify_proof_for_key(&proof, &input_key)
            .unwrap();

        // Since it's a single key, all path selectors should be `false` since
        // it's always the left-most node in each sub-tree.
        assert!(!proof.iter().any(|(_, selector)| *selector));
    }

    #[test]
    fn two_keys() {
        let key_one = "101".parse::<BigUint>().unwrap().into();
        let key_two = "100".parse::<BigUint>().unwrap().into();

        let merkle_tree = MerkleTree::new(vec![key_one, key_two]).unwrap();

        // We test that the keys were sorted by their numeric value, which means `100`
        // comes before `101` even if they were provided in the reverse order in the
        // input list.
        assert_eq!(merkle_tree.sorted_key_indices.get(&key_one), Some(&1));
        assert_eq!(merkle_tree.sorted_key_indices.get(&key_two), Some(&0));

        let proof_for_key_one = merkle_tree.get_proof_for_key(&key_one).unwrap();
        merkle_tree
            .verify_proof_for_key(&proof_for_key_one, &key_one)
            .unwrap();
        // We check that the first key is the right child of the bottom sub-tree...
        assert!(proof_for_key_one.last().unwrap().1);
        // ...but the left of all sub-trees above that.
        assert!(
            !proof_for_key_one
                .iter()
                .rev()
                .skip(1)
                .any(|(_, selector)| *selector)
        );

        let proof_for_key_two = merkle_tree.get_proof_for_key(&key_two).unwrap();
        merkle_tree
            .verify_proof_for_key(&proof_for_key_two, &key_two)
            .unwrap();
        // We check that the second key is the left child of the bottom sub-tree and all
        // sub-trees above that.
        assert!(!proof_for_key_two.iter().any(|(_, selector)| *selector));
    }

    #[test]
    fn three_keys() {
        let key_one = "101".parse::<BigUint>().unwrap().into();
        let key_two = "100".parse::<BigUint>().unwrap().into();
        let key_three = "102".parse::<BigUint>().unwrap().into();

        let merkle_tree = MerkleTree::new(vec![key_one, key_two, key_three]).unwrap();

        assert_eq!(merkle_tree.sorted_key_indices.get(&key_one), Some(&1));
        assert_eq!(merkle_tree.sorted_key_indices.get(&key_two), Some(&0));
        assert_eq!(merkle_tree.sorted_key_indices.get(&key_three), Some(&2));

        let proof_for_key_one = merkle_tree.get_proof_for_key(&key_one).unwrap();
        merkle_tree
            .verify_proof_for_key(&proof_for_key_one, &key_one)
            .unwrap();
        assert!(proof_for_key_one.last().unwrap().1);
        assert!(
            !proof_for_key_one
                .iter()
                .rev()
                .skip(1)
                .any(|(_, selector)| *selector)
        );

        let proof_for_key_two = merkle_tree.get_proof_for_key(&key_two).unwrap();
        merkle_tree
            .verify_proof_for_key(&proof_for_key_two, &key_two)
            .unwrap();
        assert!(!proof_for_key_two.iter().any(|(_, selector)| *selector));

        let proof_for_key_three = merkle_tree.get_proof_for_key(&key_three).unwrap();
        merkle_tree
            .verify_proof_for_key(&proof_for_key_three, &key_three)
            .unwrap();
        // Last selector is `false` because it's the left child...
        assert!(!proof_for_key_three.last().unwrap().1);
        // Second-to-last selector is `true` because it's already in the right sub-tree
        // at this level (first sub-tree are keys 1 and 2).
        assert!(proof_for_key_three[proof_for_key_three.len() - 2].1);
        // It's in the left-most sub-tree going above.
        assert!(
            !proof_for_key_three
                .iter()
                .rev()
                .skip(2)
                .any(|(_, selector)| *selector)
        );
    }

    #[test]
    #[ignore = "It takes too long. We might want to enable it at some point, if it makes sense."]
    fn full_keys() {
        let input_keys: Vec<_> = (0..TOTAL_MERKLE_LEAVES)
            .map(|i| fr_from_bytes_unchecked(&i.to_le_bytes()))
            .collect();
        let last_key = *input_keys.last().unwrap();
        let merkle_tree = MerkleTree::new(input_keys).unwrap();

        let proof = merkle_tree.get_proof_for_key(&last_key).unwrap();
        merkle_tree.verify_proof_for_key(&proof, &last_key).unwrap();

        // We check that the last key is the right child of all sub-trees.
        assert!(!proof.iter().any(|(_, selector)| !*selector));
    }

    #[test]
    fn empty_key_list() {
        assert_eq!(MerkleTree::new(vec![]), Err(Error::EmptyKeySet));
    }

    #[test]
    fn too_many_keys() {
        let too_many_keys = repeat_n(ZkHash::ONE, TOTAL_MERKLE_LEAVES + 1).collect::<Vec<_>>();
        assert_eq!(MerkleTree::new(too_many_keys), Err(Error::TooManyKeys));
    }

    #[test]
    fn duplicate_keys() {
        let key = ZkHash::ONE;
        assert_eq!(MerkleTree::new(vec![key, key]), Err(Error::DuplicateKey));
    }
}
