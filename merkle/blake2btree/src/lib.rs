#[cfg(test)]
pub mod test_leaf;

use blake2::{Digest as _, digest::typenum::U32};
pub use lb_dynamic_merkle::{DynamicMerkleTree, MerkleNode, MerklePath};
use lb_dynamic_merkle::{MerkleHasher, empty_subtree_root};
pub use lb_merkle_tree::Error;
use lb_merkle_tree::{LeafExtractor, MerkleTree};

pub type Hasher = blake2::Blake2b<U32>;

pub type Hash = [u8; 32];

/// Leaf value committing to an element of a [`Blake2bTree`].
///
/// The leaf binds the element's identifier together with its state, so that it
/// is meaningful on its own, independently of the position it occupies, and so
/// that mutating an element changes the root.
pub trait LeafHash<Key> {
    fn leaf_hash(&self, key: &Key) -> Hash;
}

/// [`MerkleHasher`] bridge adapting the classic blake2b hasher to the generic
/// [`DynamicMerkleTree`].
///
/// Leaves are the [`LeafHash`] of an element, computed by [`Blake2bTree`] via
/// [`Blake2bLeaf`] when the element is stored, and inner nodes are the blake2b
/// hash of their two children.
pub struct Blake2bMerkleHasher;

impl MerkleHasher for Blake2bMerkleHasher {
    type Hash = Hash;

    const EMPTY_VALUE: Hash = [0u8; 32];

    fn compress(left: &Hash, right: &Hash) -> Hash {
        let mut hasher = Hasher::new();
        hasher.update(left);
        hasher.update(right);
        hasher.finalize().into()
    }

    empty_subtree_root!(Hash);
}

/// [`LeafExtractor`] committing an element via its [`LeafHash`], with inner
/// nodes hashed by blake2b ([`Blake2bMerkleHasher`]).
pub struct Blake2bLeaf;

impl<Key, Item> LeafExtractor<Key, Item> for Blake2bLeaf
where
    Item: LeafHash<Key>,
{
    type Hasher = Blake2bMerkleHasher;

    fn leaf(key: &Key, item: &Item) -> Hash {
        item.leaf_hash(key)
    }
}

/// A [`MerkleTree`] committing to its items with blake2b (see [`LeafHash`]).
///
/// Removed items are replaced with an empty leaf, which prevents the whole tree
/// from being reordered, and their position is recorded for future insertions.
/// Updating an item replaces its leaf with another one, keeping its position.
pub type Blake2bTree<Key, Item> = MerkleTree<Key, Item, Blake2bLeaf>;

#[cfg(test)]
mod tests {
    use quickcheck::{Arbitrary, Gen};
    use quickcheck_macros::quickcheck;
    use rand::thread_rng;

    use super::*;
    use crate::test_leaf::TestLeaf;

    #[test]
    fn test_empty_tree() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
        assert_eq!(tree.size(), 0);
    }

    #[test]
    fn test_single_insert() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
        let item = TestLeaf::from_rng(&mut thread_rng());
        let key = item;
        let (tree_with_item, _pos) = tree.insert(key, item);

        assert_eq!(tree_with_item.size(), 1);
        assert_eq!(tree.size(), 0);
        assert_ne!(tree_with_item.root(), tree.root());
    }

    #[test]
    fn test_multiple_inserts() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let items = [
            TestLeaf::from_rng(&mut thread_rng()),
            TestLeaf::from_rng(&mut thread_rng()),
            TestLeaf::from_rng(&mut thread_rng()),
        ];
        let mut current_tree = tree;

        for (i, item) in items.iter().enumerate() {
            let key = item;
            let (new_tree, pos) = current_tree.insert(*key, *item);
            current_tree = new_tree;
            assert_eq!(current_tree.size(), i + 1);
            assert_eq!(pos, i);
        }

        assert_eq!(current_tree.size(), 3);
    }

    #[test]
    fn test_remove_existing_item() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let item = TestLeaf::from_rng(&mut thread_rng());
        let key = item;
        let (tree_with_item, _) = tree.insert(key, item);

        let result = tree_with_item.remove(&key);
        assert!(result.is_ok());

        let (tree_after_removal, removed_item) = result.unwrap();
        assert_eq!(tree_after_removal.size(), 0);
        assert_eq!(removed_item, item);
    }

    #[test]
    fn test_remove_non_existing_item() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let item = TestLeaf::from_rng(&mut thread_rng());
        let key = item;
        let (tree_with_item, _) = tree.insert(key, item);

        let non_existing_key = TestLeaf::from_rng(&mut thread_rng());
        let result = tree_with_item.remove(&non_existing_key);
        assert!(matches!(result, Err(Error::NotFound)));
    }

    #[test]
    fn test_remove_from_empty_tree() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let key = TestLeaf::from_rng(&mut thread_rng());
        let result = tree.remove(&key);
        assert!(matches!(result, Err(Error::NotFound)));
    }

    #[test]
    fn test_structural_sharing() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let item1 = TestLeaf::from_rng(&mut thread_rng());
        let item2 = TestLeaf::from_rng(&mut thread_rng());
        let key1 = item1;
        let key2 = item2;

        let (tree1, _) = tree.insert(key1, key1);
        let (tree2, _) = tree1.insert(key2, key2);

        assert_eq!(tree.size(), 0);
        assert_eq!(tree1.size(), 1);
        assert_eq!(tree2.size(), 2);
    }

    #[test]
    fn test_root_changes_with_operations() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let empty_root = tree.root();

        let item = TestLeaf::from_rng(&mut thread_rng());
        let key = item;
        let (tree_with_item, _) = tree.insert(key, item);
        let root_with_item = tree_with_item.root();

        assert_ne!(empty_root, root_with_item);

        let (tree_after_removal, _) = tree_with_item.remove(&key).unwrap();
        let root_after_removal = tree_after_removal.root();

        assert_eq!(empty_root, root_after_removal);
    }

    #[test]
    fn test_deterministic_root() {
        let tree1: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
        let tree2: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let items = vec![
            TestLeaf::from_rng(&mut thread_rng()),
            TestLeaf::from_rng(&mut thread_rng()),
            TestLeaf::from_rng(&mut thread_rng()),
        ];

        let mut current_tree1 = tree1;
        let mut current_tree2 = tree2;

        for item in items {
            let key = item;
            let (new_tree1, _) = current_tree1.insert(key, item);
            let (new_tree2, _) = current_tree2.insert(key, item);
            current_tree1 = new_tree1;
            current_tree2 = new_tree2;
        }

        assert_eq!(current_tree1.root(), current_tree2.root());
    }

    #[test]
    fn test_mixed_operations() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let mut current_tree = tree;
        let items = vec![
            TestLeaf::from_rng(&mut thread_rng()),
            TestLeaf::from_rng(&mut thread_rng()),
            TestLeaf::from_rng(&mut thread_rng()),
            TestLeaf::from_rng(&mut thread_rng()),
        ];

        for item in &items {
            let key = item;
            let (new_tree, _) = current_tree.insert(*key, *item);
            current_tree = new_tree;
        }
        assert_eq!(current_tree.size(), 4);

        let (tree_after_removal, _) = current_tree.remove(&items[1]).unwrap();
        assert_eq!(tree_after_removal.size(), 3);

        let (tree_after_removal2, _) = tree_after_removal.remove(&items[3]).unwrap();
        assert_eq!(tree_after_removal2.size(), 2);

        let new_item = TestLeaf::from_rng(&mut thread_rng());
        let new_key = new_item;
        let (final_tree, _) = tree_after_removal2.insert(new_key, new_item);
        assert_eq!(final_tree.size(), 3);
    }

    #[test]
    fn test_empty_tree_root_consistency() {
        let tree1: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
        let tree2: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        assert_eq!(tree1.root(), tree2.root());
    }

    #[test]
    fn test_position_tracking() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let items = vec![
            TestLeaf::from_rng(&mut thread_rng()),
            TestLeaf::from_rng(&mut thread_rng()),
            TestLeaf::from_rng(&mut thread_rng()),
        ];
        let mut current_tree = tree;
        let mut positions = Vec::new();

        for item in &items {
            let key = item;
            let (new_tree, pos) = current_tree.insert(*key, *item);
            current_tree = new_tree;
            positions.push(pos);
        }

        assert_eq!(positions, vec![0, 1, 2]);
    }

    #[test]
    fn test_large_tree_operations() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let mut current_tree = tree;
        let num_items = 100;

        for i in 0..num_items {
            let item = TestLeaf::from_usize(i);
            let key = item;
            let (new_tree, pos) = current_tree.insert(key, item);
            current_tree = new_tree;
            assert_eq!(pos, i);
        }

        assert_eq!(current_tree.size(), num_items);

        for i in (0..num_items).step_by(2) {
            let key = TestLeaf::from_usize(i);
            let result = current_tree.remove(&key);
            assert!(result.is_ok());
            let (new_tree, _) = result.unwrap();
            current_tree = new_tree;
        }

        assert_eq!(current_tree.size(), num_items / 2);
    }

    // A removed slot is reused by the next insertion, so the surviving leaves
    // keep their position instead of being compacted.
    #[test]
    fn test_removed_slot_is_reused() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let mut current_tree = tree;
        let items = (0..3).map(TestLeaf::from_usize).collect::<Vec<_>>();
        for item in &items {
            current_tree = current_tree.insert(*item, *item).0;
        }

        let (current_tree, _) = current_tree.remove(&items[1]).unwrap();
        let new_item = TestLeaf::from_usize(9);
        let (current_tree, pos) = current_tree.insert(new_item, new_item);

        assert_eq!(pos, 1);
        assert_eq!(current_tree.size(), 3);
    }

    // Leaves are never re-sorted, so the same elements inserted in a different
    // order occupy different positions and commit to a different root.
    #[test]
    fn test_root_depends_on_insertion_order() {
        let a = TestLeaf::from_usize(1);
        let b = TestLeaf::from_usize(2);

        let tree1: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
        let tree1 = tree1.insert(a, a).0.insert(b, b).0;

        let tree2: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
        let tree2 = tree2.insert(b, b).0.insert(a, a).0;

        assert_ne!(tree1.root(), tree2.root());
    }

    // The leaf commits to the item, so mutating an element changes the root
    // even though its key and position are unchanged.
    #[test]
    fn test_update_changes_root_and_keeps_position() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let mut current_tree = tree;
        let keys = (0..3).map(TestLeaf::from_usize).collect::<Vec<_>>();
        for key in &keys {
            current_tree = current_tree.insert(*key, *key).0;
        }
        let root_before = current_tree.root();

        let new_item = TestLeaf::from_usize(42);
        let updated = current_tree.update(&keys[1], new_item).unwrap();

        assert_eq!(updated.size(), 3);
        assert!(updated.contains(&keys[1]));
        assert_eq!(updated.get(&keys[1]), Some(new_item));
        assert_ne!(updated.root(), root_before);

        // The position is unchanged, so restoring the previous item restores
        // the previous root.
        let restored = updated.update(&keys[1], keys[1]).unwrap();
        assert_eq!(restored.root(), root_before);
    }

    // A removal frees the slot for the next insertion, an update does not.
    #[test]
    fn test_update_does_not_create_a_hole() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();

        let mut current_tree = tree;
        let keys = (0..3).map(TestLeaf::from_usize).collect::<Vec<_>>();
        for key in &keys {
            current_tree = current_tree.insert(*key, *key).0;
        }

        let updated = current_tree
            .update(&keys[1], TestLeaf::from_usize(42))
            .unwrap();

        let extra = TestLeaf::from_usize(43);
        let (_, pos) = updated.insert(extra, extra);
        assert_eq!(pos, 3);
    }

    #[test]
    fn test_update_non_existing_item() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
        let key = TestLeaf::from_usize(1);
        let (tree, _) = tree.insert(key, key);

        let missing = TestLeaf::from_usize(2);
        let result = tree.update(&missing, missing);
        assert!(matches!(result, Err(Error::NotFound)));
    }

    #[test]
    fn test_get_and_contains() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
        let key = TestLeaf::from_usize(1);
        let item = TestLeaf::from_usize(7);
        let (tree, _) = tree.insert(key, item);

        assert!(tree.contains(&key));
        assert!(!tree.contains(&TestLeaf::from_usize(2)));
        assert_eq!(tree.get(&key), Some(item));
        assert_eq!(tree.get(&TestLeaf::from_usize(2)), None);
        assert_eq!(tree.get_ref(&key), Some(&item));
        assert_eq!(tree.get_ref(&TestLeaf::from_usize(2)), None);
    }

    #[test]
    fn test_path_for_present_and_absent_keys() {
        let tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
        let key = TestLeaf::from_usize(1);
        let (tree, _) = tree.insert(key, key);

        assert!(tree.path(&key).is_some());
        assert!(tree.path(&TestLeaf::from_usize(2)).is_none());
    }

    // `Blake2bTree` is a type alias for the foreign `MerkleTree`, so `Arbitrary`
    // (also foreign) can't be implemented on it directly. Wrap it in a local
    // newtype for the property test.
    #[derive(Clone, Debug)]
    struct ArbTree(Blake2bTree<TestLeaf, TestLeaf>);

    impl Arbitrary for ArbTree {
        fn arbitrary(g: &mut Gen) -> Self {
            let num_items = usize::arbitrary(g) % 2 + 1;
            let mut tree: Blake2bTree<TestLeaf, TestLeaf> = Blake2bTree::new();
            let mut items = (0..num_items).map(TestLeaf::from_usize).collect::<Vec<_>>();

            for item in &items {
                let key = item;
                tree = tree.insert(*key, *item).0;
            }

            // Remove some items randomly
            let num_removals = usize::arbitrary(g) % num_items;
            for _ in 0..num_removals {
                let item = items.remove(usize::arbitrary(g) % items.len());
                tree = tree.remove(&item).unwrap().0;
            }

            Self(tree)
        }
    }

    #[quickcheck]
    fn test_compress_recover_roundtrip(test_tree: ArbTree) -> bool {
        let original_tree = test_tree.0;

        // Compress the tree
        let compressed = original_tree.compressed();

        // Recover the tree from compressed format
        let recovered_tree: Blake2bTree<_, _> = compressed.into();

        recovered_tree == original_tree && recovered_tree.root() == original_tree.root()
    }
}
