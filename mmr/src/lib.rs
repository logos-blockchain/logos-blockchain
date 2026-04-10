use std::sync::OnceLock;

use ark_ff::Field as _;
#[cfg(feature = "serde")]
use lb_groth16::serde::serde_fr;
use lb_poseidon2::{Digest, Fr};
use rpds::StackSync;

mod path;

pub use path::MerklePath;
use path::{update_paths_above_merge, update_paths_at_merge};

const EMPTY_VALUE: Fr = Fr::ZERO;
const ACCEPTABLE_MAX_HEIGHT: u8 = 32;

/// An append-only persistent Merkle Mountain Range (MMR), which can accept up
/// to 2^(`MAX_HEIGHT`-1) elements (leaves).
///
/// Compared to other merkle tree variants, this does not store leaves but
/// only the necessary internal nodes to update the root hash with new
/// additions. This makes it very space efficient, especially for large trees,
/// as we only need to store O(log n) nodes for n leaves.
///
/// Note on (de)serialization: serde will not preserve structural sharing since
/// it does not know which nodes are shared. This is ok if you only
/// (de)serialize one version of the tree, but if you dump multiple expect to
/// find multiple copes of the same nodes in the deserialized output. If you
/// need to preserve structural sharing, you should use a custom serialization.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone)]
pub struct MerkleMountainRange<T, Hash, const MAX_HEIGHT: u8 = ACCEPTABLE_MAX_HEIGHT> {
    roots: StackSync<Root>,
    #[cfg_attr(feature = "serde", serde(skip))]
    _hash: std::marker::PhantomData<(T, Hash)>,
}

impl<T, Hash, const MAX_HEIGHT: u8> PartialEq for MerkleMountainRange<T, Hash, MAX_HEIGHT> {
    fn eq(&self, other: &Self) -> bool {
        self.roots == other.roots
    }
}

impl<T, Hash, const MAX_HEIGHT: u8> Eq for MerkleMountainRange<T, Hash, MAX_HEIGHT> {}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Root {
    #[cfg_attr(feature = "serde", serde(with = "serde_fr"))]
    pub(crate) root: Fr,
    pub(crate) height: u8,
}

impl<const MAX_HEIGHT: u8, T, Hash> Default for MerkleMountainRange<T, Hash, MAX_HEIGHT>
where
    T: AsRef<Fr>,
    Hash: Digest,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<const MAX_HEIGHT: u8, T, Hash> MerkleMountainRange<T, Hash, MAX_HEIGHT>
where
    T: AsRef<Fr>,
    Hash: Digest,
{
    #[must_use]
    pub fn new() -> Self {
        assert!(
            MAX_HEIGHT <= ACCEPTABLE_MAX_HEIGHT,
            "MAX_HEIGHT must be <= {ACCEPTABLE_MAX_HEIGHT}"
        );
        Self {
            roots: StackSync::new_sync(),
            _hash: std::marker::PhantomData,
        }
    }

    pub fn push(&self, elem: T) -> Result<Self, MmrFull> {
        if self.roots.peek().is_some_and(|r| r.height == MAX_HEIGHT) {
            return Err(MmrFull);
        }

        let root = *elem.as_ref();
        let mut last_root = Root { root, height: 1 };
        let mut roots = self.roots.clone();

        while let Some(root) = roots.peek().copied() {
            if last_root.height == root.height {
                roots.pop_mut();
                last_root = Root {
                    root: Hash::compress(&[root.root, last_root.root]),
                    height: last_root.height + 1,
                };
                assert!(
                    last_root.height <= MAX_HEIGHT,
                    "Height must be less than or equal to {MAX_HEIGHT}"
                );
            } else {
                break;
            }
        }

        roots = roots.push(last_root);

        Ok(Self {
            roots,
            _hash: std::marker::PhantomData,
        })
    }

    /// Push an element, updating any tracked [`MerklePath`]s and returning
    /// a new one for the pushed element.
    ///
    /// All paths in `paths` are updated in-place so they remain valid
    /// against the new [`frontier_root`](Self::frontier_root).
    pub fn push_with_paths(
        &self,
        elem: T,
        paths: &mut [MerklePath],
    ) -> Result<(Self, MerklePath), MmrFull> {
        if self.roots.peek().is_some_and(|r| r.height == MAX_HEIGHT) {
            return Err(MmrFull);
        }

        let mut new_path = MerklePath {
            leaf_index: self.len(),
            siblings: (1..MAX_HEIGHT)
                .map(|h| empty_subtree_root::<Hash>(h))
                .collect(),
        };

        let root = *elem.as_ref();
        let mut last_root = Root { root, height: 1 };
        let mut roots = self.roots.clone();

        // Phase 1: merge same-height peaks, updating sibling hashes at each merge
        // height.
        while let Some(root) = roots.peek().copied() {
            if last_root.height == root.height {
                roots.pop_mut();
                update_paths_at_merge(last_root, root, paths, &mut new_path);
                last_root = Root {
                    root: Hash::compress(&[root.root, last_root.root]),
                    height: last_root.height + 1,
                };
                assert!(
                    last_root.height <= MAX_HEIGHT,
                    "Height must be less than or equal to {MAX_HEIGHT}"
                );
            } else {
                break;
            }
        }

        // Phase 2: update sibling hashes at heights above the merge point.
        update_paths_above_merge::<Hash, MAX_HEIGHT>(
            last_root,
            roots.iter().copied(),
            paths,
            &mut new_path,
        );

        roots = roots.push(last_root);

        Ok((
            Self {
                roots,
                _hash: std::marker::PhantomData,
            },
            new_path,
        ))
    }

    #[must_use]
    pub fn frontier_root(&self) -> Fr {
        let mut iter = self.roots.iter();

        let (mut root, mut height) = match iter.next() {
            Some(last) => (last.root, last.height),
            None => {
                // MMR is empty. Return the root of an entirely empty tree.
                return empty_subtree_root::<Hash>(MAX_HEIGHT);
            }
        };

        for last in iter {
            while height < last.height {
                root = Hash::compress(&[root, empty_subtree_root::<Hash>(height)]);
                height += 1;
            }
            root = Hash::compress(&[last.root, root]);
            height += 1;
        }

        assert!(height <= MAX_HEIGHT);
        while height < MAX_HEIGHT {
            root = Hash::compress(&[root, empty_subtree_root::<Hash>(height)]);
            height += 1;
        }
        assert_eq!(height, MAX_HEIGHT);

        root
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        Self::num_leaves(MAX_HEIGHT)
    }

    const fn num_leaves(height: u8) -> usize {
        1 << (height - 1)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.roots.iter().map(|r| Self::num_leaves(r.height)).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

pub(crate) fn empty_subtree_root<Hash: Digest>(height: u8) -> Fr {
    static PRECOMPUTED_EMPTY_ROOTS: OnceLock<[Fr; ACCEPTABLE_MAX_HEIGHT as usize]> =
        OnceLock::new();
    assert!(
        (1..=ACCEPTABLE_MAX_HEIGHT).contains(&height),
        "Height:{height} must be in 1..={ACCEPTABLE_MAX_HEIGHT}"
    );
    PRECOMPUTED_EMPTY_ROOTS.get_or_init(|| {
        let mut hashes = [EMPTY_VALUE; ACCEPTABLE_MAX_HEIGHT as usize];
        for i in 1..ACCEPTABLE_MAX_HEIGHT as usize {
            hashes[i] = Hash::compress(&[hashes[i - 1], hashes[i - 1]]);
        }
        hashes
    })[(height - 1) as usize]
}

#[derive(Debug, thiserror::Error)]
#[error("MMR is full")]
pub struct MmrFull;

#[cfg(test)]
mod test {
    use ark_ff::PrimeField as _;
    use proptest_macro::property_test;

    use super::*;
    type ZkHasher = lb_poseidon2::Poseidon2Bn254Hasher;

    struct TestFr(Fr);
    impl AsRef<Fr> for TestFr {
        fn as_ref(&self) -> &Fr {
            &self.0
        }
    }

    impl From<&[u8]> for TestFr {
        fn from(value: &[u8]) -> Self {
            Self(b2p(value))
        }
    }

    // bytes to poseidon field element
    fn b2p(b: &[u8]) -> Fr {
        let mut repr = [0u8; 32];
        assert!(b.len() <= 32);
        let len = b.len().min(32);
        repr[..len].copy_from_slice(&b[..len]);
        Fr::from_le_bytes_mod_order(&repr)
    }

    pub fn leaf(data: &[u8]) -> Fr {
        b2p(data)
    }

    #[test]
    #[expect(clippy::clone_on_copy, reason = "for the sake of the test")]
    fn test_empty_roots() {
        let mut root = Fr::ZERO;
        for i in 1..=32 {
            assert_eq!(root, empty_subtree_root::<ZkHasher>(i));
            root = <ZkHasher as Digest>::compress(&[root.clone(), root]);
        }
    }

    fn padded_leaves(elements: impl IntoIterator<Item = impl AsRef<[u8]>>, height: u8) -> Vec<Fr> {
        let mut leaves = elements
            .into_iter()
            .map(|e| leaf(e.as_ref()))
            .collect::<Vec<_>>();
        let pad = (1 << (height - 1) as usize) - leaves.len();
        leaves.extend(std::iter::repeat_n(EMPTY_VALUE, pad));
        leaves
    }

    fn root(elements: &[Fr]) -> Fr {
        let n = elements.len();
        assert!(n.is_power_of_two());
        let mut nodes = elements.to_vec();
        for h in (1..=n.ilog2()).rev() {
            for i in 0..2usize.pow(h - 1) {
                nodes[i] = <ZkHasher as Digest>::compress(&[nodes[i * 2], nodes[i * 2 + 1]]);
            }
        }

        nodes[0]
    }

    #[property_test]
    fn test_frontier_root_8(elems: Vec<[u8; 32]>) {
        let mut mmr = <MerkleMountainRange<TestFr, ZkHasher, 8>>::new();
        for elem in &elems {
            mmr = mmr.push(elem.as_ref().into()).unwrap();
        }
        assert_eq!(mmr.frontier_root(), root(&padded_leaves(elems, 8)));
    }

    #[ignore = "very slow"]
    #[property_test]
    fn test_frontier_root_16(elems: Vec<[u8; 32]>) {
        let mut mmr = <MerkleMountainRange<TestFr, ZkHasher, 16>>::new();
        for elem in &elems {
            mmr = mmr.push(elem.as_ref().into()).unwrap();
        }
        assert_eq!(mmr.frontier_root(), root(&padded_leaves(elems, 16)));
    }

    #[test]
    fn test_empty_tree() {
        let mmr = <MerkleMountainRange<TestFr, ZkHasher, 3>>::new();
        assert_eq!(mmr.len(), 0);
        assert!(mmr.is_empty());
        assert_eq!(mmr.frontier_root(), empty_subtree_root::<ZkHasher>(3));
    }

    #[test]
    #[expect(clippy::cognitive_complexity, reason = "test continuity")]
    fn test_mmr_push() {
        const HEIGHT: u8 = 3; // max 2^(3-1) = 4 leaves
        let mut mmr = <MerkleMountainRange<TestFr, ZkHasher, HEIGHT>>::new();
        assert_eq!(mmr.capacity(), 4);
        assert_eq!(mmr.len(), 0);
        let frontier_root0 = mmr.frontier_root();
        assert_eq!(frontier_root0, empty_subtree_root::<ZkHasher>(HEIGHT));

        mmr = mmr.push(b"hello".as_ref().into()).unwrap();
        assert_eq!(mmr.len(), 1);
        assert_eq!(mmr.roots.size(), 1);
        assert_eq!(mmr.roots.peek().unwrap().height, 1);
        assert_eq!(mmr.roots.peek().unwrap().root, leaf(b"hello"));
        let frontier_root1 = mmr.frontier_root();
        assert_ne!(frontier_root1, frontier_root0);

        mmr = mmr.push(b"world".as_ref().into()).unwrap();
        assert_eq!(mmr.len(), 2);
        assert_eq!(mmr.roots.size(), 1);
        assert_eq!(mmr.roots.peek().unwrap().height, 2);
        assert_eq!(
            mmr.roots.peek().unwrap().root,
            <ZkHasher as Digest>::compress(&[leaf(b"hello"), leaf(b"world")])
        );
        let frontier_root2 = mmr.frontier_root();
        assert_ne!(frontier_root2, frontier_root1);

        mmr = mmr.push(b"!".as_ref().into()).unwrap();
        assert_eq!(mmr.len(), 3);
        assert_eq!(mmr.roots.size(), 2);
        let top_root = mmr.roots.iter().last().unwrap();
        assert_eq!(top_root.height, 2);
        assert_eq!(
            top_root.root,
            <ZkHasher as Digest>::compress(&[leaf(b"hello"), leaf(b"world")])
        );
        assert_eq!(mmr.roots.peek().unwrap().height, 1);
        assert_eq!(mmr.roots.peek().unwrap().root, leaf(b"!"));
        let frontier_root3 = mmr.frontier_root();
        assert_ne!(frontier_root3, frontier_root2);

        mmr = mmr.push(b"!".as_ref().into()).unwrap();
        assert_eq!(mmr.len(), 4);
        assert_eq!(mmr.roots.size(), 1);
        assert_eq!(mmr.roots.peek().unwrap().height, 3);
        assert_eq!(
            mmr.roots.peek().unwrap().root,
            <ZkHasher as Digest>::compress(&[
                <ZkHasher as Digest>::compress(&[leaf(b"hello"), leaf(b"world")]),
                <ZkHasher as Digest>::compress(&[leaf(b"!"), leaf(b"!")])
            ])
        );
        let frontier_root4 = mmr.frontier_root();
        assert_ne!(frontier_root4, frontier_root3);

        assert!(matches!(
            mmr.push(b"already full".as_ref().into()),
            Err(MmrFull)
        ));
    }

    #[test]
    fn test_merkle_path_track_all() {
        const HEIGHT: u8 = 4; // max 8 leaves
        let elements: &[&[u8]] = &[b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"h"];
        let leaf_hashes: Vec<Fr> = elements.iter().map(|e| leaf(e)).collect();

        let mut mmr = <MerkleMountainRange<TestFr, ZkHasher, HEIGHT>>::new();
        let mut paths: Vec<MerklePath> = Vec::new();

        for (i, elem) in elements.iter().enumerate() {
            let (new_mmr, new_path) = mmr
                .push_with_paths(TestFr::from(*elem), &mut paths)
                .unwrap();
            mmr = new_mmr;
            paths.push(new_path);

            let frontier = mmr.frontier_root();
            for (j, path) in paths.iter().enumerate() {
                assert!(
                    path.verify::<ZkHasher>(leaf_hashes[j], frontier),
                    "path {j} invalid after pushing element {i}"
                );
            }
        }
    }

    #[test]
    fn test_merkle_path_track_some() {
        const HEIGHT: u8 = 4;
        let elements: &[&[u8]] = &[b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"h"];

        let mut mmr = <MerkleMountainRange<TestFr, ZkHasher, HEIGHT>>::new();
        let mut paths: Vec<MerklePath> = Vec::new();
        let mut tracked_indices: Vec<usize> = Vec::new();

        for (i, elem) in elements.iter().enumerate() {
            let (new_mmr, new_path) = mmr
                .push_with_paths(TestFr::from(*elem), &mut paths)
                .unwrap();
            mmr = new_mmr;
            // Keep only even-indexed elements.
            if i % 2 == 0 {
                tracked_indices.push(i);
                paths.push(new_path);
            }

            let frontier = mmr.frontier_root();
            for (k, path) in paths.iter().enumerate() {
                let idx = tracked_indices[k];
                assert!(
                    path.verify::<ZkHasher>(leaf(elements[idx]), frontier),
                    "path for leaf {idx} invalid after pushing element {i}"
                );
            }
        }
    }

    #[test]
    fn test_merkle_path_single_element() {
        const HEIGHT: u8 = 3;
        let mut mmr = <MerkleMountainRange<TestFr, ZkHasher, HEIGHT>>::new();
        let (new_mmr, path) = mmr
            .push_with_paths(TestFr::from(b"only".as_ref()), &mut [])
            .unwrap();
        mmr = new_mmr;
        assert_eq!(path.leaf_index(), 0);
        assert!(path.verify::<ZkHasher>(leaf(b"only"), mmr.frontier_root()));
    }

    #[test]
    fn test_merkle_path_full_tree() {
        const HEIGHT: u8 = 3; // 4 leaves
        let mut mmr = <MerkleMountainRange<TestFr, ZkHasher, HEIGHT>>::new();
        let mut paths: Vec<MerklePath> = Vec::new();
        let elems: &[&[u8]] = &[b"w", b"x", b"y", b"z"];

        for elem in elems {
            let (new_mmr, new_path) = mmr
                .push_with_paths(TestFr::from(*elem), &mut paths)
                .unwrap();
            mmr = new_mmr;
            paths.push(new_path);
        }

        // All paths should produce the same root as frontier_root.
        let frontier = mmr.frontier_root();
        let expected = root(&padded_leaves(elems.iter(), HEIGHT));
        assert_eq!(frontier, expected);
        for (i, path) in paths.iter().enumerate() {
            assert_eq!(path.root::<ZkHasher>(leaf(elems[i])), frontier);
        }

        // Tree is full — next push should fail.
        assert!(
            mmr.push_with_paths(TestFr::from(b"overflow".as_ref()), &mut paths)
                .is_err()
        );
    }

    #[test]
    fn test_push_with_paths_matches_push() {
        const HEIGHT: u8 = 8;
        let mut mmr_a = <MerkleMountainRange<TestFr, ZkHasher, HEIGHT>>::new();
        let mut mmr_b = <MerkleMountainRange<TestFr, ZkHasher, HEIGHT>>::new();

        for i in 0u64..50 {
            let bytes = i.to_le_bytes();
            mmr_a = mmr_a.push(TestFr::from(bytes.as_ref())).unwrap();
            let (new_b, _) = mmr_b
                .push_with_paths(TestFr::from(bytes.as_ref()), &mut [])
                .unwrap();
            mmr_b = new_b;
            assert_eq!(mmr_a.frontier_root(), mmr_b.frontier_root());
        }
    }
}
