mod merkle;

#[cfg(test)]
pub mod test_fr;

use std::collections::BTreeMap;

use merkle::DynamicMerkleTree;
use nomos_core::utils::merkle::MerklePath;
use poseidon2::{Digest, Fr};
use rpds::HashTrieMapSync;
use thiserror::Error;

/// A store for `UTxOs` that allows for efficient insertion, removal, and
/// retrieval of items, while efficiently maintaining a compact Merkle tree
/// for Proof of Leadership (`PoL`) generation.
///
/// Note on (de)serialization: serde will not preserve structural sharing since
/// it does not know which nodes are shared. This is ok if you only
/// (de)serialize one version of the tree, but if you dump multiple expect to
/// find multiple copes of the same nodes in the deserialized output. If you
/// need to preserve structural sharing, you should use a custom serialization.
#[derive(Debug, Clone)]
pub struct UtxoTree<Key, Item, Hash>
where
    Key: std::hash::Hash + Eq,
{
    merkle: DynamicMerkleTree<Key, Hash>,
    // key -> (item, position in merkle tree)
    items: HashTrieMapSync<Key, (Item, usize)>,
}

impl<Key, Item, Hash> Default for UtxoTree<Key, Item, Hash>
where
    Key: AsRef<Fr> + Clone + std::hash::Hash + Eq,
    Hash: Digest,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Key, Item, Hash> UtxoTree<Key, Item, Hash>
where
    Key: AsRef<Fr> + Clone + std::hash::Hash + Eq,
    Hash: Digest,
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
}

#[derive(Error, Debug, Clone)]
pub enum Error {
    #[error("Item not found")]
    NotFound,
}

impl<Key, Item, Hash> UtxoTree<Key, Item, Hash>
where
    Key: AsRef<Fr> + Clone + std::hash::Hash + Eq,
    Hash: Digest,
    Item: Clone,
{
    pub fn insert(&self, key: Key, item: Item) -> (Self, usize) {
        let (merkle, pos) = self.merkle.insert(key.clone());
        let items = self.items.insert(key, (item, pos));
        (Self { merkle, items }, pos)
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.items.contains_key(key)
    }

    #[must_use]
    pub const fn utxos(&self) -> &HashTrieMapSync<Key, (Item, usize)> {
        &self.items
    }

    pub fn remove(&self, key: &Key) -> Result<(Self, Item), Error> {
        let Some((item, pos)) = self.items.get(key) else {
            return Err(Error::NotFound);
        };
        let items = self.items.remove(key);
        let merkle = self.merkle.remove(*pos);

        Ok((Self { merkle, items }, item.clone()))
    }

    #[must_use]
    pub fn root(&self) -> Fr {
        self.merkle.root()
    }

    pub fn witness(&self, key: &Key) -> Option<()> {
        self.items.contains_key(key).then_some(())
    }

    pub fn path(&self, key: &Key) -> Option<MerklePath<Fr>> {
        let (_, pos) = self.items.get(key)?;
        self.merkle.path(*pos)
    }

    #[must_use]
    pub fn compressed(&self) -> CompressedUtxoTree<Key, Item>
    where
        Key: Clone,
        Item: Clone,
    {
        CompressedUtxoTree {
            items: self
                .items
                .iter()
                .map(|(k, (v, pos))| (*pos, (k.clone(), v.clone())))
                .collect(),
        }
    }
}

impl<Key, Item, Hash> PartialEq for UtxoTree<Key, Item, Hash>
where
    Key: AsRef<Fr> + std::hash::Hash + Eq,
    Item: PartialEq,
    Hash: Digest,
{
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items && self.merkle == other.merkle
    }
}

impl<Key, Item, Hash> Eq for UtxoTree<Key, Item, Hash>
where
    Key: AsRef<Fr> + std::hash::Hash + Eq,
    Item: Eq,
    Hash: Digest,
{
}

impl<Key, Item, Hash> FromIterator<(Key, Item)> for UtxoTree<Key, Item, Hash>
where
    Key: AsRef<Fr> + Clone + std::hash::Hash + Eq,
    Hash: Digest,
    Item: Clone,
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

impl<Key, Item, Hash> From<CompressedUtxoTree<Key, Item>> for UtxoTree<Key, Item, Hash>
where
    Key: AsRef<Fr> + Clone + std::hash::Hash + Eq,
    Hash: Digest,
    Item: Clone,
{
    fn from(compressed: CompressedUtxoTree<Key, Item>) -> Self {
        Self {
            merkle: DynamicMerkleTree::from_compressed_tree(&compressed),
            items: compressed
                .items
                .iter()
                .map(|(pos, (key, item))| (key.clone(), (item.clone(), *pos)))
                .collect(),
        }
    }
}

#[cfg_attr(
    feature = "serde",
    derive(::serde::Serialize, ::serde::Deserialize),
    serde(transparent)
)]
pub struct CompressedUtxoTree<Key, Item> {
    items: BTreeMap<usize, (Key, Item)>,
}

#[cfg(feature = "serde")]
mod serde {
    use poseidon2::{Digest, Fr};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    impl<Key, Item, Hash> Serialize for super::UtxoTree<Key, Item, Hash>
    where
        Key: Serialize + Clone + AsRef<Fr> + std::hash::Hash + Eq,
        Item: Serialize + Clone,
        Hash: Digest,
    {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            self.compressed().serialize(serializer)
        }
    }

    impl<'de, Key, Item, Hash> Deserialize<'de> for super::UtxoTree<Key, Item, Hash>
    where
        Key: AsRef<Fr> + Clone + std::hash::Hash + Eq + Deserialize<'de>,
        Item: Deserialize<'de> + Clone,
        Hash: Digest,
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let compressed = super::CompressedUtxoTree::<Key, Item>::deserialize(deserializer)?;
            Ok(compressed.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use quickcheck::{Arbitrary, Gen};
    use quickcheck_macros::quickcheck;
    use rand::rng;

    use super::*;
    use crate::test_fr::TestFr;
    type TestHash = poseidon2::Poseidon2Bn254Hasher;

    #[test]
    fn test_empty_tree() {
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();
        assert_eq!(tree.size(), 0);
    }

    #[test]
    fn test_single_insert() {
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();
        let item = TestFr::from_rng(&mut rng());
        let key = item;
        let (tree_with_item, _pos) = tree.insert(key, item);

        assert_eq!(tree_with_item.size(), 1);
        assert_eq!(tree.size(), 0);
        assert_ne!(tree_with_item.root(), tree.root());
    }

    #[test]
    fn test_multiple_inserts() {
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let items = [
            TestFr::from_rng(&mut rng()),
            TestFr::from_rng(&mut rng()),
            TestFr::from_rng(&mut rng()),
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
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let item = TestFr::from_rng(&mut rng());
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
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let item = TestFr::from_rng(&mut rng());
        let key = item;
        let (tree_with_item, _) = tree.insert(key, item);

        let non_existing_key = TestFr::from_rng(&mut rng());
        let result = tree_with_item.remove(&non_existing_key);
        assert!(matches!(result, Err(Error::NotFound)));
    }

    #[test]
    fn test_remove_from_empty_tree() {
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let key = TestFr::from_rng(&mut rng());
        let result = tree.remove(&key);
        assert!(matches!(result, Err(Error::NotFound)));
    }

    #[test]
    fn test_structural_sharing() {
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let item1 = TestFr::from_rng(&mut rng());
        let item2 = TestFr::from_rng(&mut rng());
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
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let empty_root = tree.root();

        let item = TestFr::from_rng(&mut rng());
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
        let tree1: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();
        let tree2: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let items = vec![
            TestFr::from_rng(&mut rng()),
            TestFr::from_rng(&mut rng()),
            TestFr::from_rng(&mut rng()),
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
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let mut current_tree = tree;
        let items = vec![
            TestFr::from_rng(&mut rng()),
            TestFr::from_rng(&mut rng()),
            TestFr::from_rng(&mut rng()),
            TestFr::from_rng(&mut rng()),
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

        let new_item = TestFr::from_rng(&mut rng());
        let new_key = new_item;
        let (final_tree, _) = tree_after_removal2.insert(new_key, new_item);
        assert_eq!(final_tree.size(), 3);
    }

    #[test]
    fn test_empty_tree_root_consistency() {
        let tree1: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();
        let tree2: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        assert_eq!(tree1.root(), tree2.root());
    }

    #[test]
    fn test_position_tracking() {
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let items = vec![
            TestFr::from_rng(&mut rng()),
            TestFr::from_rng(&mut rng()),
            TestFr::from_rng(&mut rng()),
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
        let tree: UtxoTree<TestFr, TestFr, TestHash> = UtxoTree::new();

        let mut current_tree = tree;
        let num_items = 100;

        for i in 0..num_items {
            let item = TestFr::from_usize(i);
            let key = item;
            let (new_tree, pos) = current_tree.insert(key, item);
            current_tree = new_tree;
            assert_eq!(pos, i);
        }

        assert_eq!(current_tree.size(), num_items);

        for i in (0..num_items).step_by(2) {
            let key = TestFr::from_usize(i);
            let result = current_tree.remove(&key);
            assert!(result.is_ok());
            let (new_tree, _) = result.unwrap();
            current_tree = new_tree;
        }

        assert_eq!(current_tree.size(), num_items / 2);
    }

    impl Arbitrary for UtxoTree<TestFr, TestFr, TestHash> {
        fn arbitrary(g: &mut Gen) -> Self {
            let num_items = usize::arbitrary(g) % 2 + 1; // 1-1000 items
            let mut tree: Self = Self::new();
            let mut items = (0..num_items).map(TestFr::from_usize).collect::<Vec<_>>();

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
    fn test_compress_recover_roundtrip(test_tree: UtxoTree<TestFr, TestFr, TestHash>) -> bool {
        let original_tree = test_tree;

        // Compress the tree
        let compressed = original_tree.compressed();

        // Recover the tree from compressed format
        let recovered_tree: UtxoTree<_, _, _> = compressed.into();

        recovered_tree == original_tree
    }
}
