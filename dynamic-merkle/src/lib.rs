//! A dynamic, persistent, fixed-height Merkle tree generic over its hashing
//! backend.
//!
//! The tree stores items at leaf positions of a binary tree of fixed height
//! ([`TREE_HEIGHT_EXCEPT_ROOT`]). Insertions fill the lowest available position
//! (reusing positions freed by removals), so an item's index is stable for the
//! lifetime of the tree and membership proofs have a constant length.
//!
//! The item type, value/hash type and hashing operations are supplied by a
//! [`MerkleHasher`] implementation, which the tree is parameterized over. Use
//! the [`empty_subtree_root`] macro to derive the cached
//! [`MerkleHasher::empty_subtree_root`] method for a concrete hash type.

use std::{fmt, marker::PhantomData, sync::Arc};

use rpds::RedBlackTreeSetSync;

/// Abstraction over the item/hash types and hashing operations a
/// [`DynamicMerkleTree`] needs.
///
/// A single implementor binds together the leaf payload type ([`Self::Item`])
/// and the field/value type stored in inner nodes, roots and paths
/// ([`Self::Hash`]).
pub trait MerkleHasher {
    /// The leaf payload stored in the tree.
    type Item: Clone;
    /// The value type: inner node values, roots and merkle-path siblings.
    type Hash: Copy + Eq;

    /// Neutral value used for empty leaves and as the seed of empty subtrees.
    const EMPTY_VALUE: Self::Hash;

    /// Extract the hash value of a leaf item.
    fn leaf_hash(item: &Self::Item) -> Self::Hash;

    /// Compress two child hashes into their parent hash.
    fn compress(left: &Self::Hash, right: &Self::Hash) -> Self::Hash;

    /// Root of a fully-empty subtree of the given `height`.
    ///
    /// Implement with [`empty_subtree_root`] to get a cached implementation.
    fn empty_subtree_root(height: usize) -> Self::Hash;
}

/// Height of the tree excluding the root, i.e. the length of every Merkle path
/// and the base-2 logarithm of the tree's leaf capacity (`2^32` items).
pub const TREE_HEIGHT_EXCEPT_ROOT: usize = 32;

/// Generates a cached [`MerkleHasher::empty_subtree_root`] implementation for a
/// concrete `Hash` type.
///
/// The cache is a `static` local to the generated method, so it is
/// monomorphization-free (the `Hash` type is concrete here) and each
/// implementing type gets its own independent cache.
///
/// ```ignore
/// impl MerkleHasher for MyHasher {
///     type Item = MyItem;
///     type Hash = Fr;
///     const EMPTY_VALUE: Fr = /* ... */;
///     fn leaf_hash(item: &MyItem) -> Fr { /* ... */ }
///     fn compress(left: &Fr, right: &Fr) -> Fr { /* ... */ }
///     empty_subtree_root!(Fr);
/// }
/// ```
#[macro_export]
macro_rules! empty_subtree_root {
    ($hash:ty) => {
        fn empty_subtree_root(height: usize) -> $hash {
            static PRECOMPUTED_EMPTY_ROOTS: ::std::sync::OnceLock<
                [$hash; $crate::TREE_HEIGHT_EXCEPT_ROOT + 1],
            > = ::std::sync::OnceLock::new();
            assert!(
                height <= $crate::TREE_HEIGHT_EXCEPT_ROOT,
                "Height{height} must be <={}",
                $crate::TREE_HEIGHT_EXCEPT_ROOT
            );
            PRECOMPUTED_EMPTY_ROOTS.get_or_init(|| {
                let mut hashes = [Self::EMPTY_VALUE; $crate::TREE_HEIGHT_EXCEPT_ROOT + 1];
                for i in 1..=$crate::TREE_HEIGHT_EXCEPT_ROOT {
                    hashes[i] = Self::compress(&hashes[i - 1], &hashes[i - 1]);
                }
                hashes
            })[height]
        }
    };
}

#[derive(::serde::Serialize, ::serde::Deserialize, Clone, Debug, PartialEq, Eq)]
enum Node<Item, Hash> {
    Inner {
        left: Arc<Self>,
        right: Arc<Self>,
        // Hash is bound to a value, not to confuse with Hasher
        value: Hash,
        right_subtree_size: usize,
        left_subtree_size: usize,
        height: usize,
    },
    // An empty inner node, representing an unexpanded empty subtree, to avoid
    // allocating a full subtree when not necessary.
    // Can only be found in the right subtree of an inner node.
    Empty {
        height: usize,
    },
    // A leaf node (possibly) containing an item, will be empty after a removal
    Leaf {
        item: Option<Item>,
    },
}

fn hash<H: MerkleHasher>(left: &Node<H::Item, H::Hash>, right: &Node<H::Item, H::Hash>) -> H::Hash {
    let left = match left {
        Node::Inner { value, .. } => *value,
        Node::Leaf { item } => item.as_ref().map_or(H::EMPTY_VALUE, H::leaf_hash),
        Node::Empty { .. } => panic!("Empty node in left subtree is not allowed"),
    };
    let right = match right {
        Node::Inner { value, .. } => *value,
        Node::Leaf { item } => item.as_ref().map_or(H::EMPTY_VALUE, H::leaf_hash),
        Node::Empty { height } => H::empty_subtree_root(*height),
    };
    H::compress(&left, &right)
}

impl<Item, Hash> Node<Item, Hash> {
    const fn new(item: Item) -> Self {
        Self::Leaf { item: Some(item) }
    }

    fn size(&self) -> usize {
        match self {
            Self::Inner {
                left_subtree_size,
                right_subtree_size,
                ..
            } => left_subtree_size + right_subtree_size,
            Self::Leaf { item: Some(_) } => 1,
            Self::Empty { .. } | Self::Leaf { item: None } => 0,
        }
    }

    // size of the full subtree
    const fn capacity(&self) -> usize {
        1 << self.height()
    }

    const fn height(&self) -> usize {
        match self {
            Self::Inner { height, .. } | Self::Empty { height } => *height,
            Self::Leaf { .. } => 0,
        }
    }
}

impl<Item, Hash: Copy> Node<Item, Hash> {
    fn new_inner<H>(left: Arc<Self>, right: Arc<Self>) -> Self
    where
        H: MerkleHasher<Item = Item, Hash = Hash>,
    {
        Self::Inner {
            right_subtree_size: right.size(),
            left_subtree_size: left.size(),
            height: left.height().max(right.height()) + 1,
            value: hash::<H>(&left, &right),
            left,
            right,
        }
    }

    fn insert_or_modify<H, F: FnOnce(&Self) -> Self>(
        self: &Arc<Self>,
        index: usize,
        f: F,
    ) -> Arc<Self>
    where
        H: MerkleHasher<Item = Item, Hash = Hash>,
    {
        match self.as_ref() {
            Self::Inner { left, right, .. } => {
                assert!(
                    index < self.capacity(),
                    "Index {} out of bounds for inner node with height {}",
                    index,
                    self.height()
                );

                if index < left.capacity() {
                    // modify the left subtree
                    Arc::new(Self::new_inner::<H>(
                        left.insert_or_modify::<H, _>(index, f),
                        Arc::clone(right),
                    ))
                } else {
                    // modify the right subtree
                    Arc::new(Self::new_inner::<H>(
                        Arc::clone(left),
                        right.insert_or_modify::<H, _>(index - left.capacity(), f),
                    ))
                }
            }
            Self::Empty { height } if *height > 0 => {
                // expand the empty subtree to modify the new item
                assert!(
                    index == 0,
                    "Cannot expand an empty subtree more than one node at a time",
                );
                Arc::new(Self::new_inner::<H>(
                    Arc::new(Self::Empty { height: height - 1 }).insert_or_modify::<H, _>(index, f),
                    Arc::new(Self::Empty { height: height - 1 }),
                ))
            }
            Self::Leaf { .. } | Self::Empty { .. } => {
                assert!(
                    index == 0,
                    "Cannot insert into a terminal node with index !=0",
                );
                Arc::new(f(self))
            }
        }
    }

    fn insert_at<H>(self: &Arc<Self>, index: usize, item: Item) -> Arc<Self>
    where
        H: MerkleHasher<Item = Item, Hash = Hash>,
    {
        self.insert_or_modify::<H, _>(index, |node| match node {
            Self::Leaf { item: None } | Self::Empty { .. } => Self::new(item),
            Self::Leaf { item: Some(_) } => panic!("Cannot insert into a non-empty leaf node"),
            _ => panic!("Cannot insert into a non-terminal node"),
        })
    }

    fn remove_at<H>(self: &Arc<Self>, index: usize) -> Arc<Self>
    where
        H: MerkleHasher<Item = Item, Hash = Hash>,
    {
        self.insert_or_modify::<H, _>(index, move |node| match node {
            Self::Leaf { item: Some(_) } => Self::Leaf { item: None },
            _ => panic!("Cannot remove from a empty / non-leaf node"),
        })
    }

    /// Computes the Merkle path for the item at the given index.
    /// The path is ordered from leaf to root (excluded).
    /// Returns `None` if the index does not exist or has been removed.
    fn path<H>(self: &Arc<Self>, index: usize) -> Option<MerklePath<Hash>>
    where
        H: MerkleHasher<Item = Item, Hash = Hash>,
    {
        match self.as_ref() {
            Self::Inner { left, right, .. } => {
                assert!(
                    index < self.capacity(),
                    "Index {} out of bounds for node with height {}",
                    index,
                    self.height()
                );

                if index < left.capacity() {
                    // Going down left subtree, store right sibling hash
                    let mut path = left.path::<H>(index)?;
                    assert!(path.len() < TREE_HEIGHT_EXCEPT_ROOT, "Path length exceeded");
                    path.push(MerkleNode::Right(right.value::<H>()));
                    Some(path)
                } else {
                    // Going down right subtree, store left sibling hash
                    let mut path = right.path::<H>(index - left.capacity())?;
                    assert!(path.len() < TREE_HEIGHT_EXCEPT_ROOT, "Path length exceeded");
                    path.push(MerkleNode::Left(left.value::<H>()));
                    Some(path)
                }
            }
            Self::Leaf { item: Some(_) } => Some(MerklePath::new()),
            Self::Leaf { item: None } | Self::Empty { .. } => None,
        }
    }

    fn value<H>(&self) -> Hash
    where
        H: MerkleHasher<Item = Item, Hash = Hash>,
    {
        match self {
            Self::Inner { value, .. } => *value,
            Self::Leaf { item: Some(item) } => H::leaf_hash(item),
            Self::Leaf { item: None } => H::EMPTY_VALUE,
            Self::Empty { height } => H::empty_subtree_root(*height),
        }
    }
}

/// A dynamic persistent Merkle tree that supports insertion and removal of
/// items.
///
/// Removed items are replaced with an empty leaf node, which prevents
/// the whole tree reordering and their position is recorded for future
/// insertions. Compared to a MPT, the height of this tree is predictable and
/// bounded by the number of items, for example allowing for efficient and
/// simple proof of memberships for `PoL`.
pub struct DynamicMerkleTree<H: MerkleHasher> {
    root: Arc<Node<H::Item, H::Hash>>,
    holes: RedBlackTreeSetSync<usize>,
    _hasher: PhantomData<H>,
}

impl<H: MerkleHasher> Clone for DynamicMerkleTree<H> {
    fn clone(&self) -> Self {
        Self {
            root: Arc::clone(&self.root),
            holes: self.holes.clone(),
            _hasher: PhantomData,
        }
    }
}

impl<H: MerkleHasher> fmt::Debug for DynamicMerkleTree<H>
where
    H::Item: fmt::Debug,
    H::Hash: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicMerkleTree")
            .field("root", &self.root)
            .field("holes", &self.holes)
            .finish()
    }
}

impl<H: MerkleHasher> Default for DynamicMerkleTree<H> {
    fn default() -> Self {
        let holes = RedBlackTreeSetSync::new_sync();
        Self {
            root: Arc::new(Node::Empty {
                height: TREE_HEIGHT_EXCEPT_ROOT,
            }),
            holes,
            _hasher: PhantomData,
        }
    }
}

impl<H: MerkleHasher> DynamicMerkleTree<H> {
    /// Creates a new, empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of items currently stored in the tree (removed
    /// positions do not count).
    #[must_use]
    pub fn size(&self) -> usize {
        self.root.size()
    }

    /// Inserts `item` at the lowest available position and returns the updated
    /// tree together with the index the item was assigned.
    ///
    /// Positions freed by [`remove`](Self::remove) are reused before the tree
    /// grows, so the smallest free index is always chosen.
    ///
    /// The original tree is left unchanged (the structure is persistent).
    ///
    /// # Panics
    ///
    /// Panics if the tree is already at full capacity
    /// (`2^TREE_HEIGHT_EXCEPT_ROOT` items).
    pub fn insert(&self, item: H::Item) -> (Self, usize) {
        assert!(
            self.size() < self.root.capacity(),
            "max capacity reached, cannot insert more items"
        );

        let (holes, index) = self.holes.first().map_or_else(
            || (self.holes.clone(), self.root.size()),
            |hole| (self.holes.remove(hole), *hole),
        );

        let root = self.root.insert_at::<H>(index, item);
        (
            Self {
                root,
                holes,
                _hasher: PhantomData,
            },
            index,
        )
    }

    /// Removes the item at `index`, returning the updated tree.
    ///
    /// The leaf is replaced with an empty one and its position is recorded as a
    /// hole for reuse by a future [`insert`](Self::insert); the tree is not
    /// otherwise restructured. The original tree is left unchanged.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds, or if the position does not hold an
    /// item.
    #[must_use]
    pub fn remove(&self, index: usize) -> Self {
        assert!(index < self.root.capacity(), "Index out of bounds");

        let root = self.root.remove_at::<H>(index);
        let holes = self.holes.insert(index);
        Self {
            root,
            holes,
            _hasher: PhantomData,
        }
    }

    /// Returns the Merkle root of the tree.
    ///
    /// An empty tree yields the empty-subtree root for the full height.
    #[must_use]
    pub fn root(&self) -> H::Hash {
        match self.root.as_ref() {
            Node::Inner { value, .. } => *value,
            Node::Leaf { .. } => {
                panic!("Cannot get root from a leaf node, expected an inner node or empty node");
            }
            Node::Empty { .. } => H::empty_subtree_root(self.root.height()),
        }
    }

    /// Computes the Merkle path for the item at the given index.
    /// The path is ordered from leaf to root (excluded).
    /// Returns `None` if the index does not exist or has been removed.
    #[must_use]
    pub fn path(&self, index: usize) -> Option<MerklePath<H::Hash>> {
        self.root.path::<H>(index).inspect(|path| {
            assert_eq!(
                path.len(),
                TREE_HEIGHT_EXCEPT_ROOT,
                "Path length({}) must be {TREE_HEIGHT_EXCEPT_ROOT}",
                path.len()
            );
        })
    }

    /// Rebuilds a tree placing each `item` at its given index, filling the gaps
    /// between indices with holes.
    ///
    /// The items must be yielded in strictly increasing index order; this is
    /// the inverse of enumerating a tree's occupied positions and is meant
    /// for recovering a tree from a compressed representation.
    ///
    /// # Panics
    ///
    /// Panics if the indices are not strictly increasing or an index is out of
    /// bounds.
    #[must_use]
    pub fn from_sorted_items(items: impl IntoIterator<Item = (usize, H::Item)>) -> Self {
        let mut tree = Self::new();
        let mut current_pos = 0;
        for (pos, item) in items {
            while current_pos < pos {
                // Insert a hole for the missing position
                tree = tree.insert_hole(current_pos);
                current_pos += 1;
            }

            tree.root = tree.root.insert_at::<H>(pos, item);
            current_pos = pos + 1;
        }
        tree
    }

    // This is only for maintaining holes information when recovering
    // the tree from a compressed format, should not be used otherwise.
    fn insert_hole(&self, index: usize) -> Self {
        assert!(
            index < self.root.capacity(),
            "Index out of bounds for inserting an empty node"
        );

        let holes = self.holes.insert(index);
        let root = self
            .root
            .insert_or_modify::<H, _>(index, |node| match node {
                Node::Empty { .. } => Node::Leaf { item: None },
                _ => panic!("Cannot insert a hole into a non-empty/non-leaf node"),
            });

        Self {
            root,
            holes,
            _hasher: PhantomData,
        }
    }
}

impl<H: MerkleHasher> PartialEq for DynamicMerkleTree<H> {
    fn eq(&self, other: &Self) -> bool {
        self.root() == other.root()
    }
}

impl<H: MerkleHasher> Eq for DynamicMerkleTree<H> {}

/// [`serde`](::serde) support for [`DynamicMerkleTree`].
///
/// The tree serializes as its root node and the set of holes; on
/// deserialization the two are reassembled into a tree. Requires the hasher's
/// [`Item`](MerkleHasher::Item) and [`Hash`](MerkleHasher::Hash) types to
/// implement the corresponding `serde` traits.
pub mod serde {
    use std::{marker::PhantomData, sync::Arc};

    use rpds::RedBlackTreeSetSync;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct as _};

    use super::MerkleHasher;

    #[derive(Deserialize)]
    struct Raw<Item, Hash> {
        root: Arc<super::Node<Item, Hash>>,
        holes: RedBlackTreeSetSync<usize>,
    }

    impl<H> Serialize for super::DynamicMerkleTree<H>
    where
        H: MerkleHasher,
        H::Item: Serialize,
        H::Hash: Serialize,
    {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut state = serializer.serialize_struct("DynamicMerkleTree", 2)?;
            state.serialize_field("root", &self.root)?;
            state.serialize_field("holes", &self.holes)?;
            state.end()
        }
    }

    impl<'de, H> Deserialize<'de> for super::DynamicMerkleTree<H>
    where
        H: MerkleHasher,
        H::Item: Deserialize<'de>,
        H::Hash: Deserialize<'de>,
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let raw = Raw::<H::Item, H::Hash>::deserialize(deserializer)?;
            Ok(Self {
                root: raw.root,
                holes: raw.holes,
                _hasher: PhantomData,
            })
        }
    }
}

/// A merkle path node indicating whether the sibling is on left or right.
#[derive(Clone)]
pub enum MerkleNode<T> {
    /// The value of sibling which is the left child.
    Left(T),
    /// The value of sibling which is the right child.
    Right(T),
}

impl<T> MerkleNode<T> {
    /// Returns the sibling value, regardless of which side it is on.
    pub const fn item(&self) -> &T {
        match self {
            Self::Left(v) | Self::Right(v) => v,
        }
    }
}

/// A Merkle path consisting of sibling nodes from leaf to root (excluded).
pub type MerklePath<T> = Vec<MerkleNode<T>>;

#[cfg(test)]
mod test_fr {
    use ark_ff::AdditiveGroup;
    use lb_poseidon2::{Digest, Fr, Poseidon2Bn254Hasher};
    use num_bigint::BigUint;
    use rand::RngCore;

    use crate::MerkleHasher;

    #[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
    pub struct TestFr(Fr);

    impl TestFr {
        pub fn from_rng<Rng: RngCore>(rng: &mut Rng) -> Self {
            Self(BigUint::from(rng.next_u64()).into())
        }

        #[must_use]
        pub fn from_usize(n: usize) -> Self {
            Self(BigUint::from(n).into())
        }
    }

    impl AsRef<Fr> for TestFr {
        fn as_ref(&self) -> &Fr {
            &self.0
        }
    }

    /// Test [`MerkleHasher`] backed by Poseidon2 over BN254.
    pub struct TestHasher;

    impl MerkleHasher for TestHasher {
        type Item = TestFr;
        type Hash = Fr;

        const EMPTY_VALUE: Fr = <Fr as AdditiveGroup>::ZERO;

        fn leaf_hash(item: &TestFr) -> Fr {
            *item.as_ref()
        }

        fn compress(left: &Fr, right: &Fr) -> Fr {
            <Poseidon2Bn254Hasher as Digest>::compress(&[*left, *right])
        }

        empty_subtree_root!(Fr);
    }
}

#[cfg(test)]
mod tests {
    use lb_poseidon2::Fr;

    use super::{
        test_fr::{TestFr, TestHasher},
        *,
    };

    #[test]
    fn test_empty_tree() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        assert_eq!(tree.size(), 0);
        assert_eq!(
            tree.root(),
            TestHasher::empty_subtree_root(TREE_HEIGHT_EXCEPT_ROOT)
        );
        assert_eq!(tree.root.height(), TREE_HEIGHT_EXCEPT_ROOT);
    }

    #[test]
    fn test_hole_management() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let mut rng = rand::thread_rng();
        let a = TestFr::from_rng(&mut rng);
        let b = TestFr::from_rng(&mut rng);
        let c = TestFr::from_rng(&mut rng);
        let d = TestFr::from_rng(&mut rng);
        let (tree1, _) = tree.insert(a);
        let (tree2, _) = tree1.insert(b);
        let (tree3, _) = tree2.insert(c);

        let tree_removed = tree3.remove(1);
        assert_eq!(tree_removed.size(), 2);

        let (tree_reinserted, index) = tree_removed.insert(d);
        assert_eq!(index, 1);
        assert_eq!(tree_reinserted.size(), 3);
    }

    #[test]
    fn test_root_consistency() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let mut rng = rand::thread_rng();
        let a = TestFr::from_rng(&mut rng);
        let b = TestFr::from_rng(&mut rng);
        let (tree1, _) = tree.insert(a);
        let (tree2, _) = tree1.insert(b);

        let root1 = tree2.root();

        let tree_removed = tree2.remove(0);
        let (tree_reinserted, _) = tree_removed.insert(a);
        let root2 = tree_reinserted.root();

        assert_eq!(root1, root2);
    }

    #[test]
    fn test_deterministic_root() {
        let mut rng = rand::thread_rng();
        let a = TestFr::from_rng(&mut rng);
        let b = TestFr::from_rng(&mut rng);
        let tree1: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let (tree1, _) = tree1.insert(a);
        let (tree1, _) = tree1.insert(b);

        let tree2: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let (tree2, _) = tree2.insert(a);
        let (tree2, _) = tree2.insert(b);

        assert_eq!(tree1.root(), tree2.root());
    }

    #[test]
    #[should_panic(expected = "Index out of bounds")]
    fn test_remove_out_of_bounds() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::thread_rng()));
        let _tree = tree.remove(1 << 32);
    }

    #[test]
    fn test_single_insert() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let item = TestFr::from_rng(&mut rand::thread_rng());
        let (tree_with_item, index) = tree.insert(item);

        assert_eq!(tree_with_item.size(), 1);
        assert_eq!(index, 0);
        assert_ne!(tree_with_item.root(), tree.root());
        assert!(matches!(tree_with_item.root.as_ref(), &Node::Inner { .. }));
    }

    #[test]
    fn test_multiple_inserts() {
        let mut tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let items = [
            TestFr::from_rng(&mut rand::thread_rng()),
            TestFr::from_rng(&mut rand::thread_rng()),
            TestFr::from_rng(&mut rand::thread_rng()),
        ];

        for (i, item) in items.iter().enumerate() {
            let (new_tree, index) = tree.insert(*item);
            tree = new_tree;
            assert_eq!(tree.size(), i + 1);
            assert_eq!(index, i);
        }

        assert_eq!(tree.size(), 3);
    }

    #[test]
    fn test_remove_single_item() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let item = TestFr::from_rng(&mut rand::thread_rng());
        let (tree_with_item, _) = tree.insert(item);

        let tree_after_removal = tree_with_item.remove(0);
        assert_eq!(tree_after_removal.size(), 0);
        assert_eq!(tree_after_removal.root(), tree.root());
    }

    #[test]
    fn test_remove_and_reinsert() {
        let mut tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let items = vec![
            TestFr::from_rng(&mut rand::thread_rng()),
            TestFr::from_rng(&mut rand::thread_rng()),
            TestFr::from_rng(&mut rand::thread_rng()),
        ];

        for item in &items {
            let (new_tree, _) = tree.insert(*item);
            tree = new_tree;
        }

        let tree_after_removal = tree.remove(1);
        assert_eq!(tree_after_removal.size(), 2);

        let (tree_after_reinsert, index) =
            tree_after_removal.insert(TestFr::from_rng(&mut rand::thread_rng()));
        assert_eq!(tree_after_reinsert.size(), 3);
        assert_eq!(index, 1);
    }

    #[test]
    fn test_structural_sharing() {
        let tree1: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let (tree2, _) = tree1.insert(TestFr::from_rng(&mut rand::thread_rng()));
        let (tree3, _) = tree2.insert(TestFr::from_rng(&mut rand::thread_rng()));

        assert_eq!(tree1.size(), 0);
        assert_eq!(tree2.size(), 1);
        assert_eq!(tree3.size(), 2);

        let tree4 = tree2.remove(0);
        assert_eq!(tree4.size(), 0);
        assert_eq!(tree2.size(), 1);
    }

    #[test]
    fn test_smallest_hole_selection() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();

        // Insert items at positions 0, 1, 2, 3, 4
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::thread_rng()));

        // Remove items at positions 3, 1, 4 (creating holes in that order)
        let tree = tree.remove(3);
        let tree = tree.remove(1);
        let tree = tree.remove(4);

        // Now we have holes at positions 1, 3, 4
        // The smallest hole should be selected first (position 1)
        let (tree, index1) = tree.insert(TestFr::from_rng(&mut rand::thread_rng()));
        assert_eq!(index1, 1, "Should select smallest hole first");

        // Next insertion should use the next smallest hole (position 3)
        let (tree, index2) = tree.insert(TestFr::from_rng(&mut rand::thread_rng()));
        assert_eq!(index2, 3, "Should select next smallest hole");

        // Final insertion should use the last hole (position 4)
        let (_, index3) = tree.insert(TestFr::from_rng(&mut rand::thread_rng()));
        assert_eq!(index3, 4, "Should select remaining hole");
    }

    #[test]
    fn test_path_empty_tree() {
        let tree = DynamicMerkleTree::<TestHasher>::new();

        // Getting a path from an empty tree should return None
        assert!(tree.path(0).is_none());
    }

    #[test]
    fn test_path_single_item() {
        let tree = DynamicMerkleTree::<TestHasher>::new();
        let item = TestFr::from_usize(0);
        let (tree, idx) = tree.insert(item);

        let path = tree.path(idx).unwrap();
        assert_eq!(path.len(), TREE_HEIGHT_EXCEPT_ROOT);

        // Verify the path can reconstruct the root
        verify_path(item, &path, tree.root());

        // For a single item at index 0, we go down the left subtree at every level
        // So all siblings should be Right nodes with empty subtree hashes
        for (height, node) in path.iter().enumerate() {
            assert!(matches!(node, MerkleNode::Right(_)));
            let sibling_hash = TestHasher::empty_subtree_root(height);
            assert_eq!(*node.item(), sibling_hash);
        }
    }

    #[test]
    fn test_path_removed_item() {
        let tree = DynamicMerkleTree::<TestHasher>::new();
        let (tree, idx) = tree.insert(TestFr::from_usize(0));

        // Path should exist before removal
        assert!(tree.path(idx).is_some());

        // Remove the item
        let tree = tree.remove(idx);
        // Path should return None after removal
        assert!(tree.path(idx).is_none());
    }

    #[test]
    fn test_path_multiple_items() {
        let tree = DynamicMerkleTree::<TestHasher>::new();
        let item0 = TestFr::from_usize(0);
        let item1 = TestFr::from_usize(1);
        let item2 = TestFr::from_usize(2);
        let (tree, idx0) = tree.insert(item0);
        let (tree, idx1) = tree.insert(item1);
        let (tree, idx2) = tree.insert(item2);

        // Test path for idx0 (leftmost item)
        let path0 = tree.path(idx0).unwrap();
        assert_eq!(path0.len(), TREE_HEIGHT_EXCEPT_ROOT);
        verify_path(item0, &path0, tree.root());

        // Test path for idx1 (second item, right sibling of idx0 at the leaf level)
        let path1 = tree.path(idx1).unwrap();
        assert_eq!(path1.len(), TREE_HEIGHT_EXCEPT_ROOT);
        verify_path(item1, &path1, tree.root());
        // For idx1, the first sibling (at leaf level) should be idx0 (left sibling)
        assert!(matches!(path1.first().unwrap(), MerkleNode::Left(_)));
        assert_eq!(*path1.first().unwrap().item(), *item0.as_ref());

        // Test path for idx2 (third item)
        let path2 = tree.path(idx2).unwrap();
        assert_eq!(path2.len(), TREE_HEIGHT_EXCEPT_ROOT);
        verify_path(item2, &path2, tree.root());
    }

    /// Verifies a Merkle path by recomputing the root hash from the leaf value
    /// and path. The path is expected to be ordered from leaf to root.
    fn verify_path(item: TestFr, path: &MerklePath<Fr>, expected_root: Fr) {
        let mut current_hash = *item.as_ref();
        for node in path {
            current_hash = match node {
                MerkleNode::Left(sibling) => TestHasher::compress(sibling, &current_hash),
                MerkleNode::Right(sibling) => TestHasher::compress(&current_hash, sibling),
            };
        }
        assert_eq!(
            current_hash, expected_root,
            "Computed root from path doesn't match expected root"
        );
    }
}
