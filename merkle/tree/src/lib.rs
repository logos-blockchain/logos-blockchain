#[cfg(test)]
pub mod test_leaf;

use std::collections::BTreeMap;

use blake2::{Digest as _, digest::typenum::U32};
pub use lb_dynamic_merkle::{DynamicMerkleTree, MerkleNode, MerklePath};
use lb_dynamic_merkle::{MerkleHasher, empty_subtree_root};
use rpds::HashTrieMapSync;
use thiserror::Error;

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
/// Leaves are the [`LeafHash`] of an element, computed by [`Blake2bTree`] when
/// the element is stored, and inner nodes are the blake2b hash of their two
/// children.
pub struct Blake2bMerkleHasher;

impl MerkleHasher for Blake2bMerkleHasher {
    type Item = Hash;
    type Hash = Hash;

    const EMPTY_VALUE: Hash = [0u8; 32];

    fn leaf_hash(item: &Hash) -> Hash {
        *item
    }

    fn compress(left: &Hash, right: &Hash) -> Hash {
        let mut hasher = Hasher::new();
        hasher.update(left);
        hasher.update(right);
        hasher.finalize().into()
    }

    empty_subtree_root!(Hash);
}

/// A store that allows for efficient insertion, update, removal, and retrieval
/// of items, while efficiently maintaining a compact Merkle tree committing to
/// them.
///
/// Removed items are replaced with an empty leaf, which prevents the whole tree
/// from being reordered, and their position is recorded for future insertions.
/// Updating an item replaces its leaf with another one, keeping its position.
///
/// Note on (de)serialization: the tree is stored in a compressed form holding
/// only the items and their positions, and the Merkle tree is rebuilt from it
/// on deserialization.
#[derive(Debug, Clone)]
pub struct Blake2bTree<Key, Item>
where
    Key: Clone + std::hash::Hash + Eq,
{
    merkle: DynamicMerkleTree<Blake2bMerkleHasher>,
    // key -> (item, position in merkle tree)
    items: HashTrieMapSync<Key, (Item, usize)>,
}

impl<Key, Item> Default for Blake2bTree<Key, Item>
where
    Key: Clone + std::hash::Hash + Eq,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Key, Item> Blake2bTree<Key, Item>
where
    Key: Clone + std::hash::Hash + Eq,
{
    #[must_use]
    pub fn new() -> Self {
        Self {
            merkle: DynamicMerkleTree::new(),
            items: HashTrieMapSync::new_sync(),
        }
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.merkle.size()
    }

    #[must_use]
    pub fn root(&self) -> Hash {
        self.merkle.root()
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.items.contains_key(key)
    }

    /// Borrows the item stored under `key`, without cloning it.
    pub fn get_ref(&self, key: &Key) -> Option<&Item> {
        self.items.get(key).map(|(item, _)| item)
    }

    #[must_use]
    pub const fn items(&self) -> &HashTrieMapSync<Key, (Item, usize)> {
        &self.items
    }

    /// Computes the Merkle path for the key.
    /// The path is ordered from leaf to root (excluded).
    /// Returns `None` if the key does not exist or has been removed.
    pub fn path(&self, key: &Key) -> Option<MerklePath<Hash>> {
        let (_, pos) = self.items.get(key)?;
        self.merkle.path(*pos)
    }
}

#[derive(Error, Debug, Clone)]
pub enum Error {
    #[error("Item not found")]
    NotFound,
}

impl<Key, Item> Blake2bTree<Key, Item>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: LeafHash<Key> + Clone,
{
    pub fn insert(&self, key: Key, item: Item) -> (Self, usize) {
        let (merkle, pos) = self.merkle.insert(item.leaf_hash(&key));
        let items = self.items.insert(key, (item, pos));
        (Self { merkle, items }, pos)
    }

    /// Replaces the item stored under `key`, keeping the position it already
    /// occupies.
    ///
    /// The leaf is replaced with the one committing to the new item rather than
    /// being emptied, so no hole is created and the surrounding leaves keep
    /// their positions.
    pub fn update(&self, key: &Key, item: Item) -> Result<Self, Error> {
        let Some((_, pos)) = self.items.get(key) else {
            return Err(Error::NotFound);
        };
        let merkle = self.merkle.update(*pos, item.leaf_hash(key));
        let items = self.items.insert(key.clone(), (item, *pos));

        Ok(Self { merkle, items })
    }

    pub fn remove(&self, key: &Key) -> Result<(Self, Item), Error> {
        let Some((item, pos)) = self.items.get(key) else {
            return Err(Error::NotFound);
        };
        let items = self.items.remove(key);
        let merkle = self.merkle.remove(*pos);

        Ok((Self { merkle, items }, item.clone()))
    }

    pub fn get(&self, key: &Key) -> Option<Item> {
        self.items.get(key).map(|(item, _)| item.clone())
    }

    #[must_use]
    pub fn compressed(&self) -> CompressedBlake2bTree<Key, Item> {
        CompressedBlake2bTree {
            items: self
                .items
                .iter()
                .map(|(k, (v, pos))| (*pos, (k.clone(), v.clone())))
                .collect(),
        }
    }
}

impl<Key, Item> PartialEq for Blake2bTree<Key, Item>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items && self.merkle == other.merkle
    }
}

impl<Key, Item> Eq for Blake2bTree<Key, Item>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: Eq,
{
}

impl<Key, Item> FromIterator<(Key, Item)> for Blake2bTree<Key, Item>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: LeafHash<Key> + Clone,
{
    fn from_iter<I: IntoIterator<Item = (Key, Item)>>(iter: I) -> Self {
        let mut tree = Self::new();
        for (key, item) in iter {
            let (new_tree, _) = tree.insert(key, item);
            tree = new_tree;
        }
        tree
    }
}

impl<Key, Item> From<CompressedBlake2bTree<Key, Item>> for Blake2bTree<Key, Item>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: LeafHash<Key> + Clone,
{
    fn from(compressed: CompressedBlake2bTree<Key, Item>) -> Self {
        // `items` is a `BTreeMap`, so iteration is ordered by position.
        let merkle = DynamicMerkleTree::from_sorted_items(
            compressed
                .items
                .iter()
                .map(|(pos, (key, item))| (*pos, item.leaf_hash(key))),
        );
        Self {
            merkle,
            items: compressed
                .items
                .iter()
                .map(|(pos, (key, item))| (key.clone(), (item.clone(), *pos)))
                .collect(),
        }
    }
}

#[derive(::serde::Serialize, ::serde::Deserialize)]
#[serde(transparent)]
pub struct CompressedBlake2bTree<Key, Item> {
    items: BTreeMap<usize, (Key, Item)>,
}

mod serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::LeafHash;

    impl<Key, Item> Serialize for super::Blake2bTree<Key, Item>
    where
        Key: Serialize + Clone + std::hash::Hash + Eq,
        Item: Serialize + LeafHash<Key> + Clone,
    {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            self.compressed().serialize(serializer)
        }
    }

    impl<'de, Key, Item> Deserialize<'de> for super::Blake2bTree<Key, Item>
    where
        Key: Clone + std::hash::Hash + Eq + Deserialize<'de>,
        Item: Deserialize<'de> + LeafHash<Key> + Clone,
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let compressed = super::CompressedBlake2bTree::<Key, Item>::deserialize(deserializer)?;
            Ok(compressed.into())
        }
    }
}

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

    impl Arbitrary for Blake2bTree<TestLeaf, TestLeaf> {
        fn arbitrary(g: &mut Gen) -> Self {
            let num_items = usize::arbitrary(g) % 2 + 1;
            let mut tree: Self = Self::new();
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

            tree
        }
    }

    #[quickcheck]
    fn test_compress_recover_roundtrip(test_tree: Blake2bTree<TestLeaf, TestLeaf>) -> bool {
        let original_tree = test_tree;

        // Compress the tree
        let compressed = original_tree.compressed();

        // Recover the tree from compressed format
        let recovered_tree: Blake2bTree<_, _> = compressed.into();

        recovered_tree == original_tree && recovered_tree.root() == original_tree.root()
    }
}
