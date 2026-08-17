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
    // An occupied leaf node.
    Leaf {
        value: Hash,
    },
}

fn hash<H: MerkleHasher>(left: &Node<H::Hash>, right: &Node<H::Hash>) -> H::Hash {
    H::compress(&left.value::<H>(), &right.value::<H>())
}

impl<Hash> Node<Hash> {
    const fn new(value: Hash) -> Self {
        Self::Leaf { value }
    }

    fn size(&self) -> usize {
        match self {
            Self::Inner {
                left_subtree_size,
                right_subtree_size,
                ..
            } => left_subtree_size + right_subtree_size,
            Self::Leaf { .. } => 1,
            Self::Empty { .. } => 0,
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
            Self::Empty { .. } => Some(0),
            Self::Leaf { .. } => None,
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
    /// Recursively validates a deserialized node tree and rebuilds derived
    /// inner-node state.
    ///
    /// Inner-node hashes, subtree sizes, and heights are recomputed from the
    /// children. Fully empty sibling subtrees are collapsed into a single
    /// empty parent. Invalid subtree heights or mismatched sibling heights
    /// are rejected.
    fn rebuild_and_validate<H>(node: &Arc<Self>) -> Result<Arc<Self>, &'static str>
    where
        H: MerkleHasher<Hash = Hash>,
    {
        let node = match node.as_ref() {
            Self::Empty { height } => {
                if *height > TREE_HEIGHT_EXCEPT_ROOT {
                    return Err("empty subtree height exceeds tree height");
                }
                return Ok(Arc::clone(node));
            }
            Self::Leaf { .. } => return Ok(Arc::clone(node)),
            Self::Inner { left, right, .. } => {
                let left = Self::rebuild_and_validate::<H>(left)?;
                let right = Self::rebuild_and_validate::<H>(right)?;
                // Malformed serialized input must be rejected as a serde
                // error; `new_inner`'s assertion is for internal misuse.
                if left.height() != right.height() {
                    return Err("sibling subtrees have different heights");
                }
                Self::new_inner::<H>(left, right)
            }
        };
        if node.height() > TREE_HEIGHT_EXCEPT_ROOT {
            return Err("tree height exceeds fixed tree height");
        }
        Ok(Arc::new(node))
    }

    fn new_inner<H>(left: Arc<Self>, right: Arc<Self>) -> Self
    where
        H: MerkleHasher<Hash = Hash>,
    {
        if let (
            Self::Empty {
                height: left_height,
            },
            Self::Empty {
                height: right_height,
            },
        ) = (left.as_ref(), right.as_ref())
        {
            assert_eq!(
                left_height, right_height,
                "empty sibling subtrees must have equal heights"
            );
            return Self::Empty {
                height: left_height + 1,
            };
        }

        assert_eq!(
            left.height(),
            right.height(),
            "sibling subtrees must have equal heights"
        );
        Self::Inner {
            right_subtree_size: right.size(),
            left_subtree_size: left.size(),
            height: left.height() + 1,
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
            Self::Empty { .. } => Self::new(value),
            Self::Leaf { .. } => panic!("Cannot insert into a non-empty leaf node"),
            Self::Inner { .. } => panic!("Cannot insert into a non-terminal node"),
        })
    }

    fn remove_at<H>(self: &Arc<Self>, index: usize) -> Arc<Self>
    where
        H: MerkleHasher<Hash = Hash>,
    {
        self.insert_or_modify::<H, _>(index, move |node| match node {
            Self::Leaf { .. } => Self::Empty { height: 0 },
            _ => panic!("Cannot remove from a empty / non-leaf node"),
        })
    }

    fn update_at<H>(self: &Arc<Self>, index: usize, value: Hash) -> Arc<Self>
    where
        H: MerkleHasher<Hash = Hash>,
    {
        self.insert_or_modify::<H, _>(index, |node| match node {
            Self::Leaf { .. } => Self::new(value),
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
            Self::Leaf { .. } => Some(Vec::new()),
            Self::Empty { .. } => None,
        }
    }

    fn value<H>(&self) -> Hash
    where
        H: MerkleHasher<Hash = Hash>,
    {
        match self {
            Self::Inner { value, .. } | Self::Leaf { value } => *value,
            Self::Empty { height } => H::empty_subtree_root(*height),
        }
    }
}

/// A dynamic persistent Merkle tree that supports insertion and removal of
/// leaf hashes.
///
/// `Node::Empty` is the sole representation of free space: height zero
/// represents one free leaf and larger heights compactly represent free
/// ranges. Sparse recovery and normal mutation therefore produce the same
/// canonical structure. Compared to a MPT, the height of this tree is
/// predictable and bounded by the number of items, for example allowing for
/// efficient and simple proof of memberships for `PoL`.
pub struct DynamicMerkleTree<H: MerkleHasher> {
    root: Arc<Node<H::Hash>>,
    _hasher: PhantomData<H>,
}

impl<H: MerkleHasher> Clone for DynamicMerkleTree<H> {
    fn clone(&self) -> Self {
        Self {
            root: Arc::clone(&self.root),
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
            .finish()
    }
}

impl<H: MerkleHasher> Default for DynamicMerkleTree<H> {
    fn default() -> Self {
        Self {
            root: Arc::new(Node::Empty {
                height: TREE_HEIGHT_EXCEPT_ROOT,
            }),
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
    /// The lowest available position is derived solely from the tree
    /// structure, so sparse recovery and removals use the same representation
    /// and the smallest free index is always chosen.
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
        let root = self.root.insert_at::<H>(index, value);
        (
            Self {
                root,
                _hasher: PhantomData,
            },
            index,
        )
    }

    /// Removes the leaf at `index`, returning the updated tree.
    ///
    /// The leaf is replaced with `Node::Empty { height: 0 }`. Empty sibling
    /// subtrees are collapsed while the ancestors are rebuilt, so the tree
    /// remains canonical. The original tree is left unchanged.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds, or if the position does not hold a
    /// leaf.
    #[must_use]
    pub fn remove(&self, index: usize) -> Self {
        assert!(index < self.root.capacity(), "Index out of bounds");

        let root = self.root.remove_at::<H>(index);
        Self {
            root,
            _hasher: PhantomData,
        }
    }

    /// Replaces the leaf hash at `index`, returning the updated tree.
    ///
    /// Unlike [`remove`](Self::remove), the leaf is replaced with another value
    /// instead of being emptied, so the position remains occupied. The
    /// original tree is left unchanged.
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
    /// gaps as empty subtrees rather than materializing individual empty
    /// leaves. Empty sibling subtrees are represented by their single empty
    /// parent, matching the canonical structure produced by mutation.
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
            "all indices must be within tree capacity"
        );
        Self {
            root,
            _hasher: PhantomData,
        }
    }

    /// Builds a sparse subtree starting at `subtree_start`, representing
    /// unoccupied ranges as [`Node::Empty`] rather than materializing
    /// individual empty leaves.
    ///
    /// A subtree of `height` covers the half-open range
    /// `[subtree_start, subtree_start + 2^height)`. Thus, `subtree_capacity =
    /// 2^height` is the width of this subtree, not an absolute index bound,
    /// and `midpoint = subtree_start + subtree_capacity / 2` divides the range
    /// into equal left and right halves.
    ///
    /// The iterator must yield entries in strictly increasing absolute
    /// position order. At `height == 0`, this subtree represents exactly one
    /// leaf position, so any consumed item is at `subtree_start`. The function
    /// consumes only entries belonging to the current subtree.
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
            let (_, value) = items.next().expect("peeked item must be available");
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
/// The tree serializes its root node. Deserialization recursively validates
/// and rebuilds the node tree, recomputing derived inner-node state and
/// collapsing fully empty sibling subtrees. Requires the hasher's
/// [`Hash`](MerkleHasher::Hash) type to implement the corresponding `serde`
/// traits.
pub mod serde {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct as _};

    use super::MerkleHasher;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw<Hash> {
        root: Arc<super::Node<Hash>>,
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
            let mut state = serializer.serialize_struct("DynamicMerkleTree", 1)?;
            state.serialize_field("root", &self.root)?;
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
            let root = super::Node::rebuild_and_validate::<H>(&raw.root)
                .map_err(serde::de::Error::custom)?;
            if root.height() != super::TREE_HEIGHT_EXCEPT_ROOT
                || matches!(root.as_ref(), super::Node::Leaf { .. })
            {
                return Err(serde::de::Error::custom(
                    "dynamic Merkle tree root has an invalid shape",
                ));
            }
            Ok(Self {
                root,
                _hasher: std::marker::PhantomData,
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
    fn test_free_position_management() {
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
    fn test_single_removal_uses_empty_leaf() {
        let tree = DynamicMerkleTree::<TestHasher>::new();
        let (tree, first_position) = tree.insert(fr_from_usize(1));
        let (tree, second_position) = tree.insert(fr_from_usize(2));

        let removed = tree.remove(first_position);

        assert_eq!(removed.size(), 1);
        assert!(matches!(
            subtree_at(&removed.root, first_position, 0),
            Node::Empty { height: 0 }
        ));
        assert!(removed.path(first_position).is_none());
        assert!(removed.path(second_position).is_some());
        assert_canonical(&removed.root);
    }

    #[test]
    fn test_sibling_empty_subtrees_collapse() {
        let mut tree = DynamicMerkleTree::<TestHasher>::new();
        for value in 0..3 {
            tree = tree.insert(fr_from_usize(value)).0;
        }

        tree = tree.remove(0);
        tree = tree.remove(1);

        assert!(matches!(
            subtree_at(&tree.root, 0, 1),
            Node::Empty { height: 1 }
        ));
        assert_eq!(tree.size(), 1);
        assert_canonical(&tree.root);
    }

    #[test]
    fn test_empty_subtrees_collapse_recursively() {
        let mut tree = DynamicMerkleTree::<TestHasher>::new();
        for value in 0..5 {
            tree = tree.insert(fr_from_usize(value)).0;
        }

        for position in 0..4 {
            tree = tree.remove(position);
        }

        assert!(matches!(
            subtree_at(&tree.root, 0, 2),
            Node::Empty { height: 2 }
        ));
        assert_eq!(tree.size(), 1);
        assert_canonical(&tree.root);
    }

    #[test]
    fn test_removing_every_item_restores_canonical_empty_tree() {
        let mut tree = DynamicMerkleTree::<TestHasher>::new();
        for value in 0..8 {
            tree = tree.insert(fr_from_usize(value)).0;
        }
        for position in 0..8 {
            tree = tree.remove(position);
        }

        assert!(matches!(
            tree.root.as_ref(),
            Node::Empty {
                height: TREE_HEIGHT_EXCEPT_ROOT
            }
        ));
        assert_eq!(tree.root(), DynamicMerkleTree::<TestHasher>::new().root());
        assert_canonical(&tree.root);
    }

    #[test]
    fn test_remove_and_reinsert_restores_root() {
        let tree = DynamicMerkleTree::<TestHasher>::new();
        let value = fr_from_usize(1);
        let (tree, _) = tree.insert(value);
        let (tree, _) = tree.insert(fr_from_usize(2));
        let original_root = tree.root();

        let tree = tree.remove(0);
        let (tree, position) = tree.insert(value);

        assert_eq!(position, 0);
        assert_eq!(tree.root(), original_root);
        assert_canonical(&tree.root);
    }

    #[test]
    fn test_mutation_and_sparse_recovery_have_same_structure() {
        let capacity = 1usize << TREE_HEIGHT_EXCEPT_ROOT;
        let items = [
            (1, fr_from_usize(1)),
            (7, fr_from_usize(7)),
            (1usize << 31, fr_from_usize(31)),
            (capacity - 1, fr_from_usize(32)),
        ];
        let recovered = DynamicMerkleTree::<TestHasher>::from_sorted_items(items);
        let mutated = tree_from_indexed_mutation(items);

        assert_eq!(mutated.size(), items.len());
        assert_eq!(mutated.root, recovered.root);
        assert_canonical(&mutated.root);
        assert_canonical(&recovered.root);
    }

    #[test]
    fn test_direct_serde_roundtrip_preserves_canonical_structure() {
        let items = [
            (1, [1; 32]),
            (7, [7; 32]),
            (1usize << 31, [31; 32]),
            ((1usize << 32) - 1, [32; 32]),
        ];
        let tree = DynamicMerkleTree::<SerdeHasher>::from_sorted_items(items);
        let (tree, inserted_position) = tree.insert([99; 32]);
        let tree = tree.remove(inserted_position);
        let serialized = serde_json::to_string(&tree).expect("serialize dynamic tree");
        let recovered: DynamicMerkleTree<SerdeHasher> =
            serde_json::from_str(&serialized).expect("deserialize dynamic tree");

        assert!(!serialized.contains("holes"));
        assert_eq!(tree.root, recovered.root);
        assert_canonical(&recovered.root);
    }

    #[test]
    fn test_direct_serde_canonicalizes_empty_inner_nodes() {
        #[derive(::serde::Serialize)]
        struct RawTree<Hash> {
            root: Arc<Node<Hash>>,
        }

        let raw = RawTree {
            root: noncanonical_empty_tree::<SerdeHasher>(TREE_HEIGHT_EXCEPT_ROOT),
        };
        let serialized = serde_json::to_string(&raw).expect("serialize non-canonical tree");
        let recovered: DynamicMerkleTree<SerdeHasher> =
            serde_json::from_str(&serialized).expect("deserialize non-canonical tree");

        assert!(matches!(
            recovered.root.as_ref(),
            Node::Empty {
                height: TREE_HEIGHT_EXCEPT_ROOT
            }
        ));
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
    fn test_smallest_free_position_selection() {
        let tree: DynamicMerkleTree<TestHasher> = DynamicMerkleTree::new();

        // Insert items at positions 0, 1, 2, 3, 4
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        let (tree, _) = tree.insert(fr_from_rng(&mut rand::thread_rng()));

        // Remove items at positions 3, 1, 4 (creating free positions in that order)
        let tree = tree.remove(3);
        let tree = tree.remove(1);
        let tree = tree.remove(4);

        // Now we have free positions at 1, 3, 4. The smallest free position
        // should be selected first (position 1).
        let (tree, index1) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        assert_eq!(index1, 1, "Should select smallest free position first");

        // Next insertion should use the next smallest free position (position 3)
        let (tree, index2) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        assert_eq!(index2, 3, "Should select next smallest free position");

        // Final insertion should use the last free position (position 4)
        let (_, index3) = tree.insert(fr_from_rng(&mut rand::thread_rng()));
        assert_eq!(index3, 4, "Should select remaining free position");
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

    fn assert_canonical<Hash>(node: &Arc<Node<Hash>>) {
        match node.as_ref() {
            Node::Empty { .. } | Node::Leaf { .. } => {}
            Node::Inner {
                left,
                right,
                left_subtree_size,
                right_subtree_size,
                height,
                ..
            } => {
                assert_eq!(left.height(), right.height());
                assert!(!matches!(
                    (left.as_ref(), right.as_ref()),
                    (Node::Empty { .. }, Node::Empty { .. })
                ));
                assert_eq!(*height, left.height() + 1);
                assert_eq!(*left_subtree_size, left.size());
                assert_eq!(*right_subtree_size, right.size());
                assert_canonical(left);
                assert_canonical(right);
            }
        }
    }

    fn subtree_at(node: &Arc<Node<Fr>>, index: usize, target_height: usize) -> &Node<Fr> {
        assert!(target_height <= node.height());
        if target_height == node.height() {
            return node;
        }
        match node.as_ref() {
            Node::Inner { left, right, .. } => {
                if index < left.capacity() {
                    subtree_at(left, index, target_height)
                } else {
                    subtree_at(right, index - left.capacity(), target_height)
                }
            }
            Node::Empty { .. } => panic!("empty subtree is already canonical at this range"),
            Node::Leaf { .. } => panic!("leaf is already at the smallest range"),
        }
    }

    fn tree_from_indexed_mutation(
        items: impl IntoIterator<Item = (usize, Fr)>,
    ) -> DynamicMerkleTree<TestHasher> {
        let root = items.into_iter().fold(
            Arc::new(Node::Empty {
                height: TREE_HEIGHT_EXCEPT_ROOT,
            }),
            |root, (position, value)| insert_at_any_position(&root, position, value),
        );
        DynamicMerkleTree {
            root,
            _hasher: PhantomData,
        }
    }

    fn insert_at_any_position(node: &Arc<Node<Fr>>, index: usize, value: Fr) -> Arc<Node<Fr>> {
        match node.as_ref() {
            Node::Inner { left, right, .. } => {
                if index < left.capacity() {
                    Arc::new(Node::new_inner::<TestHasher>(
                        insert_at_any_position(left, index, value),
                        Arc::clone(right),
                    ))
                } else {
                    Arc::new(Node::new_inner::<TestHasher>(
                        Arc::clone(left),
                        insert_at_any_position(right, index - left.capacity(), value),
                    ))
                }
            }
            Node::Empty { height } if *height > 0 => {
                let half = 1usize << (height - 1);
                let left = Arc::new(Node::Empty { height: height - 1 });
                let right = Arc::new(Node::Empty { height: height - 1 });
                if index < half {
                    Arc::new(Node::new_inner::<TestHasher>(
                        insert_at_any_position(&left, index, value),
                        right,
                    ))
                } else {
                    Arc::new(Node::new_inner::<TestHasher>(
                        left,
                        insert_at_any_position(&right, index - half, value),
                    ))
                }
            }
            Node::Empty { .. } => Arc::new(Node::new(value)),
            Node::Leaf { .. } => panic!("cannot insert into an occupied position"),
        }
    }

    fn noncanonical_empty_tree<H>(height: usize) -> Arc<Node<H::Hash>>
    where
        H: MerkleHasher,
    {
        assert!(height > 0);
        let left = if height == 1 {
            Arc::new(Node::Empty { height: 0 })
        } else {
            noncanonical_empty_tree::<H>(height - 1)
        };
        let right = Arc::new(Node::Empty { height: height - 1 });
        Arc::new(Node::Inner {
            value: hash::<H>(&left, &right),
            left_subtree_size: left.size(),
            right_subtree_size: right.size(),
            height,
            left,
            right,
        })
    }

    struct SerdeHasher;

    impl MerkleHasher for SerdeHasher {
        type Hash = [u8; 32];

        const EMPTY_VALUE: Self::Hash = [0; 32];

        fn compress(left: &Self::Hash, right: &Self::Hash) -> Self::Hash {
            let mut hash = [0; 32];
            for (output, (left, right)) in hash.iter_mut().zip(left.iter().zip(right)) {
                *output = left.wrapping_add(*right);
            }
            hash
        }

        empty_subtree_root!([u8; 32]);
    }
}
