use std::{
    marker::PhantomData,
    sync::{Arc, OnceLock},
};

use ark_ff::Field;
#[cfg(feature = "serde")]
use groth16::serde::serde_fr;
use nomos_core::utils::merkle::{MerkleNode, MerklePath};
use poseidon2::{Digest, Fr};
use rpds::RedBlackTreeSetSync;

use crate::CompressedUtxoTree;

const TREE_HEIGHT: usize = 32;
const PATH_LENGTH: usize = TREE_HEIGHT - 1; // Exclude the root

const EMPTY_VALUE: Fr = <Fr as Field>::ZERO;

fn empty_subtree_root<Hash: Digest>(height: usize) -> Fr {
    static PRECOMPUTED_EMPTY_ROOTS: OnceLock<[Fr; TREE_HEIGHT]> = OnceLock::new();
    assert!(
        height < TREE_HEIGHT,
        "Height must be less than {TREE_HEIGHT}: {height}"
    );
    PRECOMPUTED_EMPTY_ROOTS.get_or_init(|| {
        let mut hashes = [EMPTY_VALUE; TREE_HEIGHT];
        for i in 1..TREE_HEIGHT {
            hashes[i] = Hash::compress(&[hashes[i - 1], hashes[i - 1]]);
        }
        hashes
    })[height]
}

#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
enum Node<Item> {
    Inner {
        left: Arc<Self>,
        right: Arc<Self>,
        #[cfg_attr(feature = "serde", serde(with = "serde_fr"))]
        value: Fr,
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

fn hash<Item: AsRef<Fr>, Hash: Digest>(left: &Node<Item>, right: &Node<Item>) -> Fr {
    let mut input = [EMPTY_VALUE; 2];
    match left {
        Node::Inner { value, .. } => input[0] = *value,
        Node::Leaf { item } => {
            input[0] = *item.as_ref().map_or(&EMPTY_VALUE, AsRef::as_ref);
        }
        Node::Empty { .. } => panic!("Empty node in left subtree is not allowed"),
    }
    match right {
        Node::Inner { value, .. } => input[1] = *value,
        Node::Leaf { item } => {
            input[1] = *item.as_ref().map_or(&EMPTY_VALUE, AsRef::as_ref);
        }
        Node::Empty { height } => {
            input[1] = empty_subtree_root::<Hash>(*height);
        }
    }
    Hash::compress(&input)
}

impl<Item> Node<Item> {
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

impl<Item: AsRef<Fr>> Node<Item> {
    fn new_inner<Hash>(left: Arc<Self>, right: Arc<Self>) -> Self
    where
        Hash: Digest,
    {
        Self::Inner {
            right_subtree_size: right.size(),
            left_subtree_size: left.size(),
            height: left.height().max(right.height()) + 1,
            value: hash::<_, Hash>(&left, &right),
            left,
            right,
        }
    }

    fn insert_or_modify<Hash, F: FnOnce(&Self) -> Self>(
        self: &Arc<Self>,
        index: usize,
        f: F,
    ) -> Arc<Self>
    where
        Hash: Digest,
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
                    Arc::new(Self::new_inner::<Hash>(
                        left.insert_or_modify::<Hash, _>(index, f),
                        Arc::clone(right),
                    ))
                } else {
                    // modify the right subtree
                    Arc::new(Self::new_inner::<Hash>(
                        Arc::clone(left),
                        right.insert_or_modify::<Hash, _>(index - left.capacity(), f),
                    ))
                }
            }
            Self::Empty { height } if *height > 0 => {
                // expand the empty subtree to modify the new item
                assert!(
                    index == 0,
                    "Cannot expand an empty subtree more than one node at a time",
                );
                Arc::new(Self::new_inner::<Hash>(
                    Arc::new(Self::Empty { height: height - 1 })
                        .insert_or_modify::<Hash, _>(index, f),
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

    fn insert_at<Hash>(self: &Arc<Self>, index: usize, item: Item) -> Arc<Self>
    where
        Hash: Digest,
    {
        self.insert_or_modify::<Hash, _>(index, |node| match node {
            Self::Leaf { item: None } | Self::Empty { .. } => Self::new(item),
            Self::Leaf { item: Some(_) } => panic!("Cannot insert into a non-empty leaf node"),
            _ => panic!("Cannot insert into a non-terminal node"),
        })
    }

    fn remove_at<Hash>(self: &Arc<Self>, index: usize) -> Arc<Self>
    where
        Hash: Digest,
    {
        self.insert_or_modify::<Hash, _>(index, move |node| match node {
            Self::Leaf { item: Some(_) } => Self::Leaf { item: None },
            _ => panic!("Cannot remove from a empty / non-leaf node"),
        })
    }

    /// Computes the Merkle path for the item at the given index.
    ///
    /// Returns a tuple of:
    /// - The Merkle path from this node's children to the leaf
    /// - The value of this node, so the caller (parent) can wrap it with the
    ///   correct orientation.
    ///
    /// Returns `None` if the index does not exist or has been removed.
    fn path<Hash>(self: &Arc<Self>, index: usize) -> Option<(MerklePath<Fr>, Fr)>
    where
        Hash: Digest,
    {
        match self.as_ref() {
            Self::Inner {
                left, right, value, ..
            } => {
                assert!(
                    index < self.capacity(),
                    "Index {} out of bounds for node with height {}",
                    index,
                    self.height()
                );

                let mut path = MerklePath::new();

                if index < left.capacity() {
                    let (child_path, child_value) = left.path::<Hash>(index)?;
                    assert!(child_path.len() < PATH_LENGTH, "Path length exceeded");
                    path.push(MerkleNode::Left(child_value));
                    path.extend(child_path);
                } else {
                    let (child_path, child_value) = right.path::<Hash>(index - left.capacity())?;
                    assert!(child_path.len() < PATH_LENGTH, "Path length exceeded");
                    path.push(MerkleNode::Right(child_value));
                    path.extend(child_path);
                }

                Some((path, *value))
            }
            Self::Leaf { item: Some(item) } => Some((MerklePath::new(), *item.as_ref())),
            Self::Leaf { item: None } | Self::Empty { .. } => None,
        }
    }
}

/// A dynamic persistent Merkle tree that supports insertion and removal of
/// items. Removed items are replaced with an empty leaf node, which prevents
/// the whole tree reordering and their position is recorded for future
/// insertions. Compared to a MPT, the height of this tree is predictable and
/// bounded by the number of items, allowing for efficient and simple proof of
/// memberships for Proof of Leadership.
#[derive(Debug, Clone)]
pub struct DynamicMerkleTree<Item, Hash> {
    root: Arc<Node<Item>>,
    holes: RedBlackTreeSetSync<usize>,
    _hash: PhantomData<Hash>,
}

impl<Item: AsRef<Fr>, Hash: Digest> DynamicMerkleTree<Item, Hash> {
    pub fn new() -> Self {
        let holes = RedBlackTreeSetSync::new_sync();
        Self {
            root: Arc::new(Node::Empty {
                height: TREE_HEIGHT - 1,
            }),
            holes,
            _hash: PhantomData,
        }
    }

    pub fn size(&self) -> usize {
        self.root.size()
    }

    pub(crate) fn insert(&self, item: Item) -> (Self, usize) {
        assert!(
            self.size() < self.root.capacity(),
            "max capacity reached, cannot insert more items"
        );

        let (holes, index) = self.holes.first().map_or_else(
            || (self.holes.clone(), self.root.size()),
            |hole| (self.holes.remove(hole), *hole),
        );

        let root = self.root.insert_at::<Hash>(index, item);
        (
            Self {
                root,
                holes,
                _hash: PhantomData,
            },
            index,
        )
    }

    pub(crate) fn remove(&self, index: usize) -> Self {
        assert!(index < self.root.capacity(), "Index out of bounds");

        let root = self.root.remove_at::<Hash>(index);
        let holes = self.holes.insert(index);
        Self {
            root,
            holes,
            _hash: PhantomData,
        }
    }

    pub fn root(&self) -> Fr {
        match self.root.as_ref() {
            Node::Inner { value, .. } => *value,
            Node::Leaf { .. } => {
                panic!("Cannot get root from a leaf node, expected an inner node or empty node");
            }
            Node::Empty { .. } => empty_subtree_root::<Hash>(self.root.height()),
        }
    }

    /// Computes the Merkle path for the item at the given index.
    ///
    /// The returned path excludes the root and contains nodes from the root's
    /// children down to the leaf.
    ///
    /// Returns `None` if the index does not exist or has been removed.
    pub(crate) fn path(&self, index: usize) -> Option<MerklePath<Fr>> {
        self.root.path::<Hash>(index).map(|(path, _)| {
            assert_eq!(
                path.len(),
                PATH_LENGTH,
                "Path length({}) must be {PATH_LENGTH}",
                path.len()
            );
            path
        })
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
            .insert_or_modify::<Hash, _>(index, |node| match node {
                Node::Empty { .. } => Node::Leaf { item: None },
                _ => panic!("Cannot insert a hole into a non-empty/non-leaf node"),
            });

        Self {
            root,
            holes,
            _hash: PhantomData,
        }
    }
}

impl<Item: AsRef<Fr> + Clone, Hash: Digest> DynamicMerkleTree<Item, Hash> {
    pub(crate) fn from_compressed_tree<T>(comp: &CompressedUtxoTree<Item, T>) -> Self {
        let mut tree = Self::new();
        let mut current_pos = 0;
        for (pos, (key, _)) in &comp.items {
            while current_pos < *pos {
                // Insert a hole for the missing position
                tree = tree.insert_hole(current_pos);
                current_pos += 1;
            }

            tree.root = tree.root.insert_at::<Hash>(*pos, key.clone());
            current_pos = *pos + 1;
        }
        tree
    }
}

impl<Item, Hash> PartialEq for DynamicMerkleTree<Item, Hash>
where
    Item: AsRef<Fr> + PartialEq,
    Hash: Digest,
{
    fn eq(&self, other: &Self) -> bool {
        self.root() == other.root()
    }
}

impl<Item, Hash> Eq for DynamicMerkleTree<Item, Hash>
where
    Item: AsRef<Fr> + Eq,
    Hash: Digest,
{
}

#[cfg(feature = "serde")]
pub mod serde {
    use std::{marker::PhantomData, sync::Arc};

    use rpds::RedBlackTreeSetSync;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct as _};

    #[derive(Deserialize)]
    pub struct DynamicMerkleTree<Item> {
        root: Arc<super::Node<Item>>,
        holes: RedBlackTreeSetSync<usize>,
    }

    impl<Item, Hash> From<DynamicMerkleTree<Item>> for super::DynamicMerkleTree<Item, Hash> {
        fn from(tree: DynamicMerkleTree<Item>) -> Self {
            Self {
                root: tree.root,
                holes: tree.holes,
                _hash: PhantomData,
            }
        }
    }

    impl<Item, Hash> Serialize for super::DynamicMerkleTree<Item, Hash>
    where
        Item: Serialize,
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

    impl<'de, Item, Hash> Deserialize<'de> for super::DynamicMerkleTree<Item, Hash>
    where
        Item: Deserialize<'de>,
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let raw: DynamicMerkleTree<Item> = Deserialize::deserialize(deserializer)?;
            Ok(raw.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fr::TestFr;

    type TestHash = poseidon2::Poseidon2Bn254Hasher;

    #[test]
    fn test_empty_tree() {
        let tree: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        assert_eq!(tree.size(), 0);
        assert_eq!(tree.root(), empty_subtree_root::<TestHash>(TREE_HEIGHT - 1));
    }

    #[test]
    fn test_hole_management() {
        let tree: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let mut rng = rand::rng();
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
        let tree: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let mut rng = rand::rng();
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
        let mut rng = rand::rng();
        let a = TestFr::from_rng(&mut rng);
        let b = TestFr::from_rng(&mut rng);
        let tree1: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let (tree1, _) = tree1.insert(a);
        let (tree1, _) = tree1.insert(b);

        let tree2: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let (tree2, _) = tree2.insert(a);
        let (tree2, _) = tree2.insert(b);

        assert_eq!(tree1.root(), tree2.root());
    }

    #[test]
    #[should_panic(expected = "Index out of bounds")]
    fn test_remove_out_of_bounds() {
        let tree: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::rng()));
        tree.remove(1 << 32);
    }

    #[test]
    fn test_single_insert() {
        let tree: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let item = TestFr::from_rng(&mut rand::rng());
        let (tree_with_item, index) = tree.insert(item);

        assert_eq!(tree_with_item.size(), 1);
        assert_eq!(index, 0);
        assert_ne!(tree_with_item.root(), tree.root());
        assert!(matches!(tree_with_item.root.as_ref(), &Node::Inner { .. }));
    }

    #[test]
    fn test_multiple_inserts() {
        let mut tree: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let items = [
            TestFr::from_rng(&mut rand::rng()),
            TestFr::from_rng(&mut rand::rng()),
            TestFr::from_rng(&mut rand::rng()),
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
        let tree: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let item = TestFr::from_rng(&mut rand::rng());
        let (tree_with_item, _) = tree.insert(item);

        let tree_after_removal = tree_with_item.remove(0);
        assert_eq!(tree_after_removal.size(), 0);
        assert_eq!(tree_after_removal.root(), tree.root());
    }

    #[test]
    fn test_remove_and_reinsert() {
        let mut tree: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let items = vec![
            TestFr::from_rng(&mut rand::rng()),
            TestFr::from_rng(&mut rand::rng()),
            TestFr::from_rng(&mut rand::rng()),
        ];

        for item in &items {
            let (new_tree, _) = tree.insert(*item);
            tree = new_tree;
        }

        let tree_after_removal = tree.remove(1);
        assert_eq!(tree_after_removal.size(), 2);

        let (tree_after_reinsert, index) =
            tree_after_removal.insert(TestFr::from_rng(&mut rand::rng()));
        assert_eq!(tree_after_reinsert.size(), 3);
        assert_eq!(index, 1);
    }

    #[test]
    fn test_structural_sharing() {
        let tree1: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();
        let (tree2, _) = tree1.insert(TestFr::from_rng(&mut rand::rng()));
        let (tree3, _) = tree2.insert(TestFr::from_rng(&mut rand::rng()));

        assert_eq!(tree1.size(), 0);
        assert_eq!(tree2.size(), 1);
        assert_eq!(tree3.size(), 2);

        let tree4 = tree2.remove(0);
        assert_eq!(tree4.size(), 0);
        assert_eq!(tree2.size(), 1);
    }

    #[test]
    fn test_smallest_hole_selection() {
        let tree: DynamicMerkleTree<TestFr, TestHash> = DynamicMerkleTree::new();

        // Insert items at positions 0, 1, 2, 3, 4
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::rng()));
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::rng()));
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::rng()));
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::rng()));
        let (tree, _) = tree.insert(TestFr::from_rng(&mut rand::rng()));

        // Remove items at positions 3, 1, 4 (creating holes in that order)
        let tree = tree.remove(3);
        let tree = tree.remove(1);
        let tree = tree.remove(4);

        // Now we have holes at positions 1, 3, 4
        // The smallest hole should be selected first (position 1)
        let (tree, index1) = tree.insert(TestFr::from_rng(&mut rand::rng()));
        assert_eq!(index1, 1, "Should select smallest hole first");

        // Next insertion should use the next smallest hole (position 3)
        let (tree, index2) = tree.insert(TestFr::from_rng(&mut rand::rng()));
        assert_eq!(index2, 3, "Should select next smallest hole");

        // Final insertion should use the last hole (position 4)
        let (_, index3) = tree.insert(TestFr::from_rng(&mut rand::rng()));
        assert_eq!(index3, 4, "Should select remaining hole");
    }

    #[test]
    fn test_path_empty_tree() {
        let tree = DynamicMerkleTree::<TestFr, TestHash>::new();

        // Getting a path from an empty tree should return None
        assert!(tree.path(0).is_none());
    }

    #[test]
    fn test_path_single_item() {
        let tree = DynamicMerkleTree::<TestFr, TestHash>::new();
        let item = TestFr::from_usize(0);
        let (tree, idx) = tree.insert(item);

        let path = tree.path(idx).unwrap();
        // Should have path from root down to leaf
        assert_eq!(path.len(), PATH_LENGTH);

        // Check that all should be Left nodes with expected hashes
        // by traversing the path backward from the leaf.
        let mut expected_hash = *item.as_ref(); // init with the leaf value
        for (height, node) in path.iter().rev().enumerate() {
            assert!(matches!(node, MerkleNode::Left(_)));
            assert_eq!(*node.item(), expected_hash);

            // Update expected_hash
            let sibling_hash = empty_subtree_root::<TestHash>(height);
            expected_hash = <TestHash as Digest>::compress(&[expected_hash, sibling_hash]);
        }
        // The computed hash should match the tree root
        assert_eq!(expected_hash, tree.root());
    }

    #[test]
    fn test_path_removed_item() {
        let tree = DynamicMerkleTree::<TestFr, TestHash>::new();
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
        let tree = DynamicMerkleTree::<TestFr, TestHash>::new();
        let item0 = TestFr::from_usize(0);
        let item1 = TestFr::from_usize(1);
        let item2 = TestFr::from_usize(2);
        let (tree, idx0) = tree.insert(item0);
        let (tree, idx1) = tree.insert(item1);
        let (tree, idx2) = tree.insert(item2);

        // Test path for idx0
        let path = tree.path(idx0).unwrap();
        assert_eq!(path.len(), PATH_LENGTH);
        for node in &path {
            assert!(matches!(node, MerkleNode::Left(_)));
        }
        // Verify the leaf value
        assert_eq!(*path.last().unwrap().item(), *item0.as_ref());

        // Test path for idx1
        let path = tree.path(idx1).unwrap();
        assert_eq!(path.len(), PATH_LENGTH);
        let mut iter = path.iter();
        for _ in 0..(path.len() - 1) {
            assert!(matches!(iter.next().unwrap(), MerkleNode::Left(_)));
        }
        // Verify the leaf node
        let last_node = iter.next().unwrap();
        assert!(matches!(last_node, MerkleNode::Right(_)));
        assert_eq!(*last_node.item(), *item1.as_ref());

        // Test path for idx2
        let path = tree.path(idx2).unwrap();
        assert_eq!(path.len(), PATH_LENGTH);
        let mut iter = path.iter();
        for _ in 0..(path.len() - 2) {
            assert!(matches!(iter.next().unwrap(), MerkleNode::Left(_)));
        }
        assert!(matches!(iter.next().unwrap(), MerkleNode::Right(_)));
        // Verify the leaf node
        let last_node = iter.next().unwrap();
        assert!(matches!(last_node, MerkleNode::Left(_)));
        assert_eq!(*last_node.item(), *item2.as_ref());
    }
}
