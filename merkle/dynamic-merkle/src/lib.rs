//! A dynamic, persistent, fixed-height Merkle tree generic over its hashing
//! backend.
//!
//! The tree stores leaf hashes at positions of a binary tree of fixed height
//! ([`TREE_HEIGHT_EXCEPT_ROOT`]). Insertions fill the lowest available position
//! (reusing positions freed by removals), so a leaf's index is stable for the
//! lifetime of the tree and membership proofs have a constant length.
//!
//! The value/hash type and hashing operations are supplied by a
//! [`MerkleHasher`] implementation, which the tree is parameterized over. How a
//! leaf hash is derived from application data is the caller's concern: the tree
//! only ever sees the already-computed [`MerkleHasher::Hash`] values. Use the
//! [`empty_subtree_root`] macro to derive the cached
//! [`MerkleHasher::empty_subtree_root`] method for a concrete hash type.

use std::{fmt, marker::PhantomData, sync::Arc};

use rpds::RedBlackTreeSetSync;

/// Abstraction over the hash type and hashing operations a
/// [`DynamicMerkleTree`] needs.
///
/// The tree is a tree of hashes: leaves already hold a [`Self::Hash`] (the
/// caller decides how to derive it from application data), so the hasher only
/// needs to know how to combine two children and what an empty value is.
pub trait MerkleHasher {
    /// The value type: leaf values, inner node values, roots and merkle-path
    /// siblings.
    type Hash: Copy + Eq;

    /// Neutral value used for empty leaves and as the seed of empty subtrees.
    const EMPTY_VALUE: Self::Hash;

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
///     type Hash = Fr;
///     const EMPTY_VALUE: Fr = /* ... */;
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
enum Node<Hash> {
    Inner {
        left: Arc<Self>,
        right: Arc<Self>,
        // Hash is bound to a value, not to confuse with Hasher
        value: Hash,
        right_subtree_size: usize,
        left_subtree_size: usize,
        height: usize,
    },
    // An unexpanded, fully-empty subtree. Height zero represents one empty
    // leaf position; larger heights compactly represent multiple empty
    // leaves, avoiding allocations for ranges that are not occupied.
    Empty {
        height: usize,
    },
    // A leaf node (possibly) holding a hash, will be empty after a removal
    Leaf {
        value: Option<Hash>,
    },
}

fn hash<H: MerkleHasher>(left: &Node<H::Hash>, right: &Node<H::Hash>) -> H::Hash {
    H::compress(&left.value::<H>(), &right.value::<H>())
}

impl<Hash> Node<Hash> {
    const fn new(value: Hash) -> Self {
        Self::Leaf { value: Some(value) }
    }

    fn size(&self) -> usize {
        match self {
            Self::Inner {
                left_subtree_size,
                right_subtree_size,
                ..
            } => left_subtree_size + right_subtree_size,
            Self::Leaf { value: Some(_) } => 1,
            Self::Empty { .. } | Self::Leaf { value: None } => 0,
        }
    }

    // size of the full subtree
    const fn capacity(&self) -> usize {
        1 << self.height()
    }

    fn first_empty_index(&self) -> Option<usize> {
        match self {
            Self::Inner { left, right, .. } => {
                if left.size() < left.capacity() {
                    left.first_empty_index()
                } else if right.size() < right.capacity() {
                    right
                        .first_empty_index()
                        .map(|index| left.capacity() + index)
                } else {
                    None
                }
            }
            Self::Empty { .. } | Self::Leaf { value: None } => Some(0),
            Self::Leaf { value: Some(_) } => None,
        }
    }

    const fn height(&self) -> usize {
        match self {
            Self::Inner { height, .. } | Self::Empty { height } => *height,
            Self::Leaf { .. } => 0,
        }
    }
}

impl<Hash: Copy> Node<Hash> {
    fn new_inner<H>(left: Arc<Self>, right: Arc<Self>) -> Self
    where
        H: MerkleHasher<Hash = Hash>,
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
        H: MerkleHasher<Hash = Hash>,
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

    fn insert_at<H>(self: &Arc<Self>, index: usize, value: Hash) -> Arc<Self>
    where
        H: MerkleHasher<Hash = Hash>,
    {
        self.insert_or_modify::<H, _>(index, |node| match node {
            Self::Leaf { value: None } | Self::Empty { .. } => Self::new(value),
            Self::Leaf { value: Some(_) } => panic!("Cannot insert into a non-empty leaf node"),
            _ => panic!("Cannot insert into a non-terminal node"),
        })
    }

    fn remove_at<H>(self: &Arc<Self>, index: usize) -> Arc<Self>
    where
        H: MerkleHasher<Hash = Hash>,
    {
        self.insert_or_modify::<H, _>(index, move |node| match node {
            Self::Leaf { value: Some(_) } => Self::Leaf { value: None },
            _ => panic!("Cannot remove from a empty / non-leaf node"),
        })
    }

    fn update_at<H>(self: &Arc<Self>, index: usize, value: Hash) -> Arc<Self>
    where
        H: MerkleHasher<Hash = Hash>,
    {
        self.insert_or_modify::<H, _>(index, |node| match node {
            Self::Leaf { value: Some(_) } => Self::new(value),
            _ => panic!("Cannot update an empty / non-leaf node"),
        })
    }

    /// Computes the Merkle path for the item at the given index.
    /// The path is ordered from leaf to root (excluded).
    /// Returns `None` if the index does not exist or has been removed.
    fn path<H>(self: &Arc<Self>, index: usize) -> Option<Vec<MerkleNode<Hash>>>
    where
        H: MerkleHasher<Hash = Hash>,
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
                    if path.len() >= TREE_HEIGHT_EXCEPT_ROOT {
                        return None;
                    }
                    path.push(MerkleNode::Right(right.value::<H>()));
                    Some(path)
                } else {
                    // Going down right subtree, store left sibling hash
                    let mut path = right.path::<H>(index - left.capacity())?;
                    if path.len() >= TREE_HEIGHT_EXCEPT_ROOT {
                        return None;
                    }
                    path.push(MerkleNode::Left(left.value::<H>()));
                    Some(path)
                }
            }
            Self::Leaf { value: Some(_) } => Some(Vec::new()),
            Self::Leaf { value: None } | Self::Empty { .. } => None,
        }
    }

    fn value<H>(&self) -> Hash
    where
        H: MerkleHasher<Hash = Hash>,
    {
        match self {
            Self::Inner { value, .. } | Self::Leaf { value: Some(value) } => *value,
            Self::Leaf { value: None } => H::EMPTY_VALUE,
            Self::Empty { height } => H::empty_subtree_root(*height),
        }
    }
}

/// A dynamic persistent Merkle tree that supports insertion and removal of
/// leaf hashes.
///
/// Removed leaves are replaced with an explicit `Leaf { value: None }`, which
/// prevents reordering of the whole tree; their positions are recorded for
/// future insertions. Sparse recovery represents missing ranges structurally
/// with `Node::Empty` instead of creating one empty leaf per missing position.
/// Positions freed by removal remain tracked in `holes` for the tree's
/// existing serialization and bookkeeping behavior. Compared to a MPT, the
/// height of this tree is predictable and bounded by the number of items, for
/// example allowing for efficient and simple proof of memberships for `PoL`.
pub struct DynamicMerkleTree<H: MerkleHasher> {
    root: Arc<Node<H::Hash>>,
    // Explicit empty leaves created by remove(). Sparse gaps are represented
    // by Node::Empty in root instead; keep this set for removal bookkeeping
    // and the existing serialized representation.
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

    /// Returns the number of leaves currently stored in the tree (removed
    /// positions do not count).
    #[must_use]
    pub fn size(&self) -> usize {
        self.root.size()
    }

    /// Inserts the leaf hash `value` at the lowest available position and
    /// returns the updated tree together with the index it was assigned.
    ///
    /// The lowest available position is derived from the tree structure, so
    /// this works for both implicit empty ranges from sparse recovery and
    /// explicit empty leaves created by [`remove`](Self::remove). Positions
    /// freed by removal are reused before the tree grows, so the smallest free
    /// index is always chosen. If the selected position is in `holes`, it is
    /// removed from that bookkeeping set.
    ///
    /// The original tree is left unchanged (the structure is persistent).
    ///
    /// # Panics
    ///
    /// Panics if the tree is already at full capacity
    /// (`2^TREE_HEIGHT_EXCEPT_ROOT` items).
    pub fn insert(&self, value: H::Hash) -> (Self, usize) {
        assert!(
            self.size() < self.root.capacity(),
            "max capacity reached, cannot insert more items"
        );

        let index = self
            .root
            .first_empty_index()
            .expect("tree has capacity but no empty position");
        let holes = self.holes.remove(&index);

        let root = self.root.insert_at::<H>(index, value);
        (
            Self {
                root,
                holes,
                _hasher: PhantomData,
            },
            index,
        )
    }

    /// Removes the leaf at `index`, returning the updated tree.
    ///
    /// The leaf is replaced with an explicit empty leaf and its position is
    /// recorded as a hole for reuse by a future [`insert`](Self::insert); the
    /// tree is not otherwise restructured. The original tree is left
    /// unchanged.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds, or if the position does not hold a
    /// leaf.
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

    /// Replaces the leaf hash at `index`, returning the updated tree.
    ///
    /// Unlike [`remove`](Self::remove), the leaf is replaced with another value
    /// instead of being emptied, so the position is neither freed nor recorded
    /// as a hole. The original tree is left unchanged.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds, or if the position does not hold a
    /// leaf.
    #[must_use]
    pub fn update(&self, index: usize, value: H::Hash) -> Self {
        assert!(index < self.root.capacity(), "Index out of bounds");

        let root = self.root.update_at::<H>(index, value);
        Self {
            root,
            holes: self.holes.clone(),
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

    /// Computes the Merkle path for the leaf at the given index.
    /// The path is ordered from leaf to root (excluded).
    /// Returns `None` if the index does not exist or has been removed.
    #[must_use]
    pub fn path(&self, index: usize) -> Option<MerklePath<H::Hash>> {
        self.root.path::<H>(index)?.try_into().ok()
    }

    /// Rebuilds a tree placing each leaf hash at its given index, representing
    /// gaps as implicit empty subtrees rather than explicit holes. Those gaps
    /// are therefore stored as `Node::Empty` ranges in the tree structure;
    /// the `holes` set remains reserved for positions explicitly freed by
    /// [`remove`](Self::remove).
    ///
    /// The values must be yielded in strictly increasing index order; this is
    /// the inverse of enumerating a tree's occupied positions and is meant
    /// for recovering a tree from a compressed representation.
    ///
    /// # Panics
    ///
    /// Panics if the indices are not strictly increasing or an index is out of
    /// bounds.
    #[must_use]
    pub fn from_sorted_items(items: impl IntoIterator<Item = (usize, H::Hash)>) -> Self {
        let mut items = items.into_iter().peekable();
        let root = Self::build_sparse_subtree(&mut items, 0, TREE_HEIGHT_EXCEPT_ROOT);
        assert!(
            items.next().is_none(),
            "indices must be strictly increasing and within bounds"
        );
        Self {
            root,
            holes: RedBlackTreeSetSync::new_sync(),
            _hasher: PhantomData,
        }
    }

    /// Builds the subtree whose first absolute leaf position is
    /// `subtree_start`.
    ///
    /// A subtree of `height` covers the half-open range
    /// `[subtree_start, subtree_start + 2^height)`. Thus, `subtree_capacity =
    /// 2^height` is the width of this subtree, not an absolute index bound,
    /// and `midpoint = subtree_start + subtree_capacity / 2` divides the range
    /// into equal left and right halves.
    ///
    /// The iterator must yield entries in strictly increasing absolute
    /// position order. At `height == 0`, this subtree represents exactly one
    /// leaf position, so the consumed item must have `position ==
    /// subtree_start`. The function consumes only entries belonging to the
    /// current subtree and represents unoccupied ranges with
    /// [`Node::Empty`] instead of materializing individual empty leaves.
    fn build_sparse_subtree<I>(
        items: &mut std::iter::Peekable<I>,
        subtree_start: usize,
        height: usize,
    ) -> Arc<Node<H::Hash>>
    where
        I: Iterator<Item = (usize, H::Hash)>,
    {
        let Some(&(position, _)) = items.peek() else {
            return Arc::new(Node::Empty { height });
        };
        let subtree_capacity = 1usize << height;
        assert!(
            position >= subtree_start && position - subtree_start < subtree_capacity,
            "indices must be strictly increasing and within bounds"
        );
        if height == 0 {
            let (position, value) = items.next().expect("peeked item must be available");
            assert_eq!(
                position, subtree_start,
                "indices must be strictly increasing"
            );
            return Arc::new(Node::new(value));
        }

        let midpoint = subtree_start + (subtree_capacity / 2);
        let left = if items
            .peek()
            .is_some_and(|(position, _)| *position < midpoint)
        {
            Self::build_sparse_subtree(items, subtree_start, height - 1)
        } else {
            Arc::new(Node::Empty { height: height - 1 })
        };
        let right = if items
            .peek()
            .is_some_and(|(position, _)| *position < subtree_start + subtree_capacity)
        {
            Self::build_sparse_subtree(items, midpoint, height - 1)
        } else {
            Arc::new(Node::Empty { height: height - 1 })
        };
        Arc::new(Node::new_inner::<H>(left, right))
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
/// [`Hash`](MerkleHasher::Hash) type to implement the corresponding `serde`
/// traits.
pub mod serde {
    use std::{marker::PhantomData, sync::Arc};

    use rpds::RedBlackTreeSetSync;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct as _};

    use super::MerkleHasher;

    #[derive(Deserialize)]
    struct Raw<Hash> {
        root: Arc<super::Node<Hash>>,
        holes: RedBlackTreeSetSync<usize>,
    }

    impl<H> Serialize for super::DynamicMerkleTree<H>
    where
        H: MerkleHasher,
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
        H::Hash: Deserialize<'de>,
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let raw = Raw::<H::Hash>::deserialize(deserializer)?;
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
pub type MerklePath<T> = [MerkleNode<T>; TREE_HEIGHT_EXCEPT_ROOT];

#[cfg(test)]
mod test_fr {
    use ark_ff::AdditiveGroup;
    use lb_poseidon2::{Digest, Fr, Poseidon2Bn254Hasher};
    use num_bigint::BigUint;
    use rand::RngCore;

    use crate::MerkleHasher;

    pub fn fr_from_rng<Rng: RngCore>(rng: &mut Rng) -> Fr {
        BigUint::from(rng.next_u64()).into()
    }

    #[must_use]
    pub fn fr_from_usize(n: usize) -> Fr {
        BigUint::from(n).into()
    }

    /// Test [`MerkleHasher`] backed by Poseidon2 over BN254.
    pub struct TestHasher;

    impl MerkleHasher for TestHasher {
        type Hash = Fr;

        const EMPTY_VALUE: Fr = <Fr as AdditiveGroup>::ZERO;

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
        test_fr::{TestHasher, fr_from_rng, fr_from_usize},
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
        let a = fr_from_rng(&mut rng);
        let b = fr_from_rng(&mut rng);
        let c = fr_from_rng(&mut rng);
        let d = fr_from_rng(&mut rng);
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
        let a = fr_from_rng(&mut rng);
        let b = fr_from_rng(&mut rng);
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
        let a = fr_from_rng(&mut rng);
        let b = fr_from_rng(&mut rng);
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
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        let _tree = tree.remove(1 << 32);
    }

    #[test]
    fn test_single_insert() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let item = fr_from_rng(&mut rand::thread_rng());
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
            fr_from_rng(&mut rand::thread_rng()),
            fr_from_rng(&mut rand::thread_rng()),
            fr_from_rng(&mut rand::thread_rng()),
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
    fn test_sparse_recovery_does_not_materialize_gaps() {
        let value = fr_from_usize(1);
        let recovered =
            DynamicMerkleTree::<TestHasher>::from_sorted_items([((1usize << 31) + 1, value)]);

        assert_eq!(recovered.size(), 1);
        let (recovered, index) = recovered.insert(fr_from_usize(2));
        assert_eq!(index, 0);
        assert_eq!(recovered.size(), 2);
    }

    #[test]
    fn test_remove_single_item() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let item = fr_from_rng(&mut rand::thread_rng());
        let (tree_with_item, _) = tree.insert(item);

        let tree_after_removal = tree_with_item.remove(0);
        assert_eq!(tree_after_removal.size(), 0);
        assert_eq!(tree_after_removal.root(), tree.root());
    }

    #[test]
    fn test_remove_and_reinsert() {
        let mut tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let items = vec![
            fr_from_rng(&mut rand::thread_rng()),
            fr_from_rng(&mut rand::thread_rng()),
            fr_from_rng(&mut rand::thread_rng()),
        ];

        for item in &items {
            let (new_tree, _) = tree.insert(*item);
            tree = new_tree;
        }

        let tree_after_removal = tree.remove(1);
        assert_eq!(tree_after_removal.size(), 2);

        let (tree_after_reinsert, index) =
            tree_after_removal.insert(fr_from_rng(&mut rand::thread_rng()));
        assert_eq!(tree_after_reinsert.size(), 3);
        assert_eq!(index, 1);
    }

    #[test]
    fn test_structural_sharing() {
        let tree1: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();
        let (tree2, _) = tree1.insert(fr_from_rng(&mut rand::thread_rng()));
        let (tree3, _) = tree2.insert(fr_from_rng(&mut rand::thread_rng()));

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
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));

        // Remove items at positions 3, 1, 4 (creating holes in that order)
        let tree = tree.remove(3);
        let tree = tree.remove(1);
        let tree = tree.remove(4);

        // Now we have holes at positions 1, 3, 4
        // The smallest hole should be selected first (position 1)
        let (tree, index1) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        assert_eq!(index1, 1, "Should select smallest hole first");

        // Next insertion should use the next smallest hole (position 3)
        let (tree, index2) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        assert_eq!(index2, 3, "Should select next smallest hole");

        // Final insertion should use the last hole (position 4)
        let (_, index3) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
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
        let item = fr_from_usize(0);
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
        let (tree, idx) = tree.insert(fr_from_usize(0));

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
        let item0 = fr_from_usize(0);
        let item1 = fr_from_usize(1);
        let item2 = fr_from_usize(2);
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
        assert_eq!(*path1.first().unwrap().item(), item0);

        // Test path for idx2 (third item)
        let path2 = tree.path(idx2).unwrap();
        assert_eq!(path2.len(), TREE_HEIGHT_EXCEPT_ROOT);
        verify_path(item2, &path2, tree.root());
    }

    /// Verifies a Merkle path by recomputing the root hash from the leaf value
    /// and path. The path is expected to be ordered from leaf to root.
    fn verify_path(item: Fr, path: &MerklePath<Fr>, expected_root: Fr) {
        let mut current_hash = item;
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
