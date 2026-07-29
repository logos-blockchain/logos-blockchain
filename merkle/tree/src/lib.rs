use std::{collections::BTreeMap, fmt, marker::PhantomData};

use lb_dynamic_merkle::MerkleHasher;
pub use lb_dynamic_merkle::{DynamicMerkleTree, MerkleNode, MerklePath};
use rpds::HashTrieMapSync;
use thiserror::Error;

/// Derives the Merkle leaf payload committed for an element from its key and
/// value, and ties the store to a concrete [`MerkleHasher`].
///
/// Implementors are zero-sized "strategy" markers: they carry no state and only
/// decide how a `(key, item)` pair maps to the leaf payload stored in the
/// underlying [`DynamicMerkleTree`], and which hasher backs it. See the
/// `blake2btree` and `utxotree` crates for concrete extractors.
pub trait LeafExtractor<Key, Item> {
    /// The hashing backend of the underlying [`DynamicMerkleTree`].
    type Hasher: MerkleHasher;

    /// Builds the leaf hash stored for `(key, item)`.
    fn leaf(key: &Key, item: &Item) -> <Self::Hasher as MerkleHasher>::Hash;
}

/// A store that allows for efficient insertion, update, removal, and retrieval
/// of items, while efficiently maintaining a compact Merkle tree committing to
/// them.
///
/// The way an item is committed to (the leaf payload and the hasher) is
/// supplied by a [`LeafExtractor`] `Leaf`, so the same orchestration serves
/// different Merkle flavours (e.g. `Blake2bTree`, or `UtxoTree`).
///
/// Removed items are replaced with an empty leaf, which prevents the whole tree
/// from being reordered, and their position is recorded for future insertions.
/// Updating an item replaces its leaf with another one, keeping its position.
///
/// Note on (de)serialization: the tree is stored in a compressed form holding
/// only the items and their positions, and the Merkle tree is rebuilt from it
/// on deserialization.
pub struct MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq,
    Leaf: LeafExtractor<Key, Item>,
{
    merkle: DynamicMerkleTree<Leaf::Hasher>,
    // key -> (item, position in merkle tree)
    items: HashTrieMapSync<Key, (Item, usize)>,
    _leaf: PhantomData<Leaf>,
}

impl<Key, Item, Leaf> Clone for MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq,
    Leaf: LeafExtractor<Key, Item>,
{
    fn clone(&self) -> Self {
        Self {
            merkle: self.merkle.clone(),
            items: self.items.clone(),
            _leaf: PhantomData,
        }
    }
}

impl<Key, Item, Leaf> fmt::Debug for MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq + fmt::Debug,
    Item: fmt::Debug,
    Leaf: LeafExtractor<Key, Item>,
    DynamicMerkleTree<Leaf::Hasher>: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MerkleTree")
            .field("merkle", &self.merkle)
            .field("items", &self.items)
            .finish()
    }
}

impl<Key, Item, Leaf> Default for MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq,
    Leaf: LeafExtractor<Key, Item>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Key, Item, Leaf> MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq,
    Leaf: LeafExtractor<Key, Item>,
{
    #[must_use]
    pub fn new() -> Self {
        Self {
            merkle: DynamicMerkleTree::new(),
            items: HashTrieMapSync::new_sync(),
            _leaf: PhantomData,
        }
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.merkle.size()
    }

    #[must_use]
    pub fn root(&self) -> <Leaf::Hasher as MerkleHasher>::Hash {
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

    /// Iterates over the stored `(key, item)` pairs, in no particular order
    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Item)> {
        self.items.iter().map(|(key, (item, _))| (key, item))
    }

    /// Computes the Merkle path for the key.
    /// The path is ordered from leaf to root (excluded).
    /// Returns `None` if the key does not exist or has been removed.
    pub fn path(&self, key: &Key) -> Option<MerklePath<<Leaf::Hasher as MerkleHasher>::Hash>> {
        let (_, pos) = self.items.get(key)?;
        self.merkle.path(*pos)
    }
}

#[derive(Error, Debug, Clone)]
pub enum Error {
    #[error("Item not found")]
    NotFound,
}

impl<Key, Item, Leaf> MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: Clone,
    Leaf: LeafExtractor<Key, Item>,
{
    pub fn insert(&self, key: Key, item: Item) -> (Self, usize) {
        let (merkle, pos) = self.merkle.insert(Leaf::leaf(&key, &item));
        let items = self.items.insert(key, (item, pos));
        (
            Self {
                merkle,
                items,
                _leaf: PhantomData,
            },
            pos,
        )
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
        let merkle = self.merkle.update(*pos, Leaf::leaf(key, &item));
        let items = self.items.insert(key.clone(), (item, *pos));

        Ok(Self {
            merkle,
            items,
            _leaf: PhantomData,
        })
    }

    pub fn remove(&self, key: &Key) -> Result<(Self, Item), Error> {
        let Some((item, pos)) = self.items.get(key) else {
            return Err(Error::NotFound);
        };
        let items = self.items.remove(key);
        let merkle = self.merkle.remove(*pos);

        Ok((
            Self {
                merkle,
                items,
                _leaf: PhantomData,
            },
            item.clone(),
        ))
    }

    pub fn get(&self, key: &Key) -> Option<Item> {
        self.items.get(key).map(|(item, _)| item.clone())
    }

    #[must_use]
    pub fn compressed(&self) -> CompressedMerkleTree<Key, Item> {
        CompressedMerkleTree {
            items: self
                .items
                .iter()
                .map(|(k, (v, pos))| (*pos, (k.clone(), v.clone())))
                .collect(),
        }
    }
}

impl<Key, Item, Leaf> PartialEq for MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: PartialEq,
    Leaf: LeafExtractor<Key, Item>,
{
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items && self.merkle == other.merkle
    }
}

impl<Key, Item, Leaf> Eq for MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: Eq,
    Leaf: LeafExtractor<Key, Item>,
{
}

impl<Key, Item, Leaf> FromIterator<(Key, Item)> for MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: Clone,
    Leaf: LeafExtractor<Key, Item>,
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

impl<Key, Item, Leaf> From<CompressedMerkleTree<Key, Item>> for MerkleTree<Key, Item, Leaf>
where
    Key: Clone + std::hash::Hash + Eq,
    Item: Clone,
    Leaf: LeafExtractor<Key, Item>,
{
    fn from(compressed: CompressedMerkleTree<Key, Item>) -> Self {
        // `items` is a `BTreeMap`, so iteration is ordered by position.
        let merkle = DynamicMerkleTree::from_sorted_items(
            compressed
                .items
                .iter()
                .map(|(pos, (key, item))| (*pos, Leaf::leaf(key, item))),
        );
        Self {
            merkle,
            items: compressed
                .items
                .iter()
                .map(|(pos, (key, item))| (key.clone(), (item.clone(), *pos)))
                .collect(),
            _leaf: PhantomData,
        }
    }
}

#[derive(::serde::Serialize, ::serde::Deserialize)]
#[serde(transparent)]
pub struct CompressedMerkleTree<Key, Item> {
    items: BTreeMap<usize, (Key, Item)>,
}

mod serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::LeafExtractor;

    impl<Key, Item, Leaf> Serialize for super::MerkleTree<Key, Item, Leaf>
    where
        Key: Serialize + Clone + std::hash::Hash + Eq,
        Item: Serialize + Clone,
        Leaf: LeafExtractor<Key, Item>,
    {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            self.compressed().serialize(serializer)
        }
    }

    impl<'de, Key, Item, Leaf> Deserialize<'de> for super::MerkleTree<Key, Item, Leaf>
    where
        Key: Clone + std::hash::Hash + Eq + Deserialize<'de>,
        Item: Deserialize<'de> + Clone,
        Leaf: LeafExtractor<Key, Item>,
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let compressed = super::CompressedMerkleTree::<Key, Item>::deserialize(deserializer)?;
            Ok(compressed.into())
        }
    }
}
