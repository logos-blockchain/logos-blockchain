use lb_poseidon2::{Digest, Fr};
use serde::{Deserialize, Serialize};

use crate::{Root, empty_subtree_root};

/// A merkle inclusion proof for a leaf in an MMR.
///
/// Contains the sibling hashes along the path from leaf to root. Can be used
/// to verify that a leaf is included under a given frontier root, or to
/// recompute the root from a leaf hash.
///
/// Paths are created via [`crate::MerkleMountainRange::push_with_paths`] and
/// kept up-to-date by passing them to subsequent `push_with_paths` calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerklePath {
    /// The 0-indexed leaf position in the tree.
    pub leaf_index: usize,
    /// Sibling hashes from height 1 (bottom) up to height `MAX_HEIGHT - 1`.
    /// `siblings[h - 1]` is the root of the sibling subtree at height `h`.
    #[serde(with = "lb_groth16::serde::serde_fr_vec")]
    pub siblings: Vec<Fr>,
}

impl MerklePath {
    /// Compute the merkle root from a leaf hash and this proof.
    #[must_use]
    pub fn root<Hash: Digest>(&self, leaf: Fr) -> Fr {
        let mut current = leaf;
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

    /// Verify that `leaf` is included under `expected_root`.
    #[must_use]
    pub fn verify<Hash: Digest>(&self, leaf: Fr, expected_root: Fr) -> bool {
        self.root::<Hash>(leaf) == expected_root
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
///
/// Time complexity: O(p), where p = number of tracked paths
pub fn update_paths_at_merge(
    right: Root,
    left: Root,
    paths: &mut [MerklePath],
    new_path: &mut MerklePath,
) {
    assert_eq!(
        left.height, right.height,
        "merge requires same-height peaks: left={}, right={}",
        left.height, right.height
    );
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
///
/// Time complexity: O(`MAX_HEIGHT` + p), where p = number of tracked paths.
pub fn update_paths_above_merge<Hash: Digest, const MAX_HEIGHT: u8>(
    merged_root: Root,
    mut remaining_peaks: impl Iterator<Item = Root>,
    paths: &mut [MerklePath],
    new_path: &mut MerklePath,
) {
    let merged_height = merged_root.height as usize;
    let mut subtree_hash = merged_root.root;

    // Phase 1: precompute subtree hashes at each height and update new_path.
    let height_range = merged_height..MAX_HEIGHT as usize;
    let mut hashes = Vec::with_capacity(height_range.len());
    for height in height_range.clone() {
        hashes.push(subtree_hash);

        if is_left_child(new_path.leaf_index, height) {
            let empty = empty_subtree_root::<Hash>(height as u8);
            new_path.siblings[height - 1] = empty;
            subtree_hash = Hash::compress(&[subtree_hash, empty]);
        } else {
            let left = remaining_peaks.next().expect("stack underflow");
            new_path.siblings[height - 1] = left.root;
            subtree_hash = Hash::compress(&[left.root, subtree_hash]);
        }
    }

    // Phase 2: update each path at its unique sibling height.
    for path in paths.iter_mut() {
        let h = sibling_height(new_path.leaf_index, path.leaf_index);
        if height_range.contains(&h) {
            path.siblings[h - 1] = hashes[h - merged_height];
        }
    }
}

/// Whether `leaf` sits in the left subtree at the given tree `height`.
#[must_use]
pub const fn is_left_child(leaf: usize, height: usize) -> bool {
    (leaf >> (height - 1)) & 1 == 0
}

/// Whether leaves `a` and `b` belong to sibling subtrees at the given tree
/// `height` (i.e. they share a common ancestor at `height + 1` but fall into
/// different subtrees at `height`).
const fn are_siblings_at(a: usize, b: usize, height: usize) -> bool {
    (a >> (height - 1)) == (b >> (height - 1)) ^ 1
}

/// The unique height at which leaves `a` and `b` are siblings.
const fn sibling_height(a: usize, b: usize) -> usize {
    (usize::BITS - (a ^ b).leading_zeros()) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    //       h=3
    //      /    \
    //    h=2    h=2
    //   /  \   /  \
    //  h=1 h=1 h=1 h=1
    //  0 1 2 3 4 5 6 7

    #[test]
    fn test_is_left_child() {
        assert!(is_left_child(0, 1));
        assert!(!is_left_child(1, 1));
        assert!(is_left_child(2, 1));
        assert!(!is_left_child(3, 1));

        assert!(is_left_child(0, 2));
        assert!(is_left_child(1, 2));
        assert!(!is_left_child(2, 2));
        assert!(!is_left_child(3, 2));

        assert!(is_left_child(0, 3));
        assert!(is_left_child(3, 3));
        assert!(!is_left_child(4, 3));
        assert!(!is_left_child(7, 3));
    }

    #[test]
    fn test_are_siblings_at() {
        // Leaves 0,1 are siblings at h=1
        assert!(are_siblings_at(0, 1, 1));
        assert!(are_siblings_at(1, 0, 1));
        // Leaves 2,3 are siblings at h=1
        assert!(are_siblings_at(2, 3, 1));
        // Leaves 0,1 are NOT siblings at h=2
        assert!(!are_siblings_at(0, 1, 2));
        // Leaves 0,2 are siblings at h=2 (subtrees [0,1] and [2,3])
        assert!(are_siblings_at(0, 2, 2));
        assert!(are_siblings_at(1, 3, 2));
        // Leaves 0,4 are siblings at h=3 (subtrees [0..3] and [4..7])
        assert!(are_siblings_at(0, 4, 3));
        assert!(are_siblings_at(3, 7, 3));
        assert!(!are_siblings_at(0, 4, 2));
    }

    #[test]
    fn test_sibling_height() {
        // Adjacent pairs → h=1
        assert_eq!(sibling_height(0, 1), 1);
        assert_eq!(sibling_height(6, 7), 1);
        // Across h=2 subtrees
        assert_eq!(sibling_height(0, 2), 2);
        assert_eq!(sibling_height(1, 3), 2);
        // Across h=3 subtrees
        assert_eq!(sibling_height(0, 4), 3);
        assert_eq!(sibling_height(3, 7), 3);
    }
}
