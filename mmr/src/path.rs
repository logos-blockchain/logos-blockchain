use lb_poseidon2::{Digest, Fr};

use crate::{Root, empty_subtree_root};

/// A merkle inclusion proof for a leaf in an MMR.
///
/// Contains the sibling hashes along the path from leaf to root. Can be used
/// to verify that a leaf is included under a given frontier root, or to
/// recompute the root from a leaf hash.
///
/// Paths are created via [`crate::MerkleMountainRange::push_with_paths`] and
/// kept up-to-date by passing them to subsequent `push_with_paths` calls.
#[derive(Debug, Clone)]
pub struct MerklePath {
    /// The 0-indexed leaf position in the tree.
    pub leaf_index: usize,
    /// Sibling hashes from height 1 (bottom) up to height `MAX_HEIGHT - 1`.
    /// `siblings[h - 1]` is the root of the sibling subtree at height `h`.
    pub siblings: Vec<Fr>,
}

impl MerklePath {
    /// Compute the merkle root from a leaf hash and this proof.
    #[must_use]
    pub fn root<Hash: Digest>(&self, leaf_hash: Fr) -> Fr {
        let mut current = leaf_hash;
        for (h, &sibling) in self.siblings.iter().enumerate() {
            let height = h + 1;
            if is_left_child(self.leaf_index, height) {
                current = Hash::compress(&[current, sibling]);
            } else {
                current = Hash::compress(&[sibling, current]);
            }
        }
        current
    }

    /// Verify that `leaf_hash` is included under `expected_root`.
    #[must_use]
    pub fn verify<Hash: Digest>(&self, leaf_hash: Fr, expected_root: Fr) -> bool {
        self.root::<Hash>(leaf_hash) == expected_root
    }

    /// The 0-indexed leaf position this path corresponds to.
    #[must_use]
    pub const fn leaf_index(&self) -> usize {
        self.leaf_index
    }
}

/// Update paths during a merge step.
///
/// `right` is the subtree being merged from the right (containing the new
/// leaf), and `left` is the existing peak being popped from the stack. At this
/// height, `right.root` is the sibling for any tracked leaf on the left side,
/// and `left.root` is the sibling for the new leaf's path.
pub fn update_paths_at_merge(
    right: Root,
    left: Root,
    paths: &mut [MerklePath],
    new_path: &mut MerklePath,
) {
    let height = right.height as usize;

    for path in paths.iter_mut() {
        if are_siblings_at(new_path.leaf_index, path.leaf_index, height) {
            path.siblings[height - 1] = right.root;
        }
    }

    new_path.siblings[height - 1] = left.root;
}

/// Update paths for heights above the merge point.
///
/// After the merge loop in [`crate::MerkleMountainRange::push_with_paths`],
/// the subtree containing the new leaf may still be a sibling of tracked leaves
/// at greater heights. This function walks upward, computing the growing
/// subtree root by combining with remaining peaks (left siblings) or empty
/// subtree roots (right siblings).
pub fn update_paths_above_merge<Hash: Digest, const MAX_HEIGHT: u8>(
    merged_root: Root,
    remaining_peaks: impl Iterator<Item = Root>,
    paths: &mut [MerklePath],
    new_path: &mut MerklePath,
) {
    let mut subtree_hash = merged_root.root;
    let mut peaks = remaining_peaks;

    for height in merged_root.height as usize..MAX_HEIGHT as usize {
        for path in paths.iter_mut() {
            if are_siblings_at(new_path.leaf_index, path.leaf_index, height) {
                path.siblings[height - 1] = subtree_hash;
            }
        }

        if is_left_child(new_path.leaf_index, height) {
            let empty = empty_subtree_root::<Hash>(height as u8);
            new_path.siblings[height - 1] = empty;
            subtree_hash = Hash::compress(&[subtree_hash, empty]);
        } else {
            let left = peaks.next().expect("stack underflow");
            new_path.siblings[height - 1] = left.root;
            subtree_hash = Hash::compress(&[left.root, subtree_hash]);
        }
    }
}

/// Whether `leaf` sits in the left subtree at the given tree `height`.
const fn is_left_child(leaf: usize, height: usize) -> bool {
    (leaf >> (height - 1)) & 1 == 0
}

/// Whether leaves `a` and `b` belong to sibling subtrees at the given tree
/// `height` (i.e. they share a common ancestor at `height + 1` but fall into
/// different subtrees at `height`).
const fn are_siblings_at(a: usize, b: usize, height: usize) -> bool {
    (a >> (height - 1)) == (b >> (height - 1)) ^ 1
}
