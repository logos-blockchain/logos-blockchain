use std::sync::OnceLock;

use ark_ff::Field as _;
#[cfg(feature = "serde")]
use lb_groth16::serde::serde_fr;
use lb_poseidon2::{Digest, Fr};
use rpds::StackSync;

const EMPTY_VALUE: Fr = Fr::ZERO;

/// An append-only persistent Merkle Mountain Range (MMR).
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
pub struct MerkleMountainRange<T, Hash, const MAX_HEIGHT: u8 = 32> {
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
struct Root {
    #[cfg_attr(feature = "serde", serde(with = "serde_fr"))]
    root: Fr,
    height: u8,
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
            MAX_HEIGHT <= 32,
            "MAX_HEIGHT must be less than or equal to 32"
        );
        Self {
            roots: StackSync::new_sync(),
            _hash: std::marker::PhantomData,
        }
    }

    #[must_use]
    pub fn push(&self, elem: T) -> Self {
        let root = Hash::digest(&[*elem.as_ref()]);
        let mut last_root = Root { root, height: 1 };
        let mut roots = self.roots.clone();

        while let Some(root) = roots.peek().copied() {
            if last_root.height == root.height {
                roots.pop_mut();
                last_root = Root {
                    root: Hash::compress(&[root.root, last_root.root]),
                    height: last_root.height + 1,
                };
                // we want the frontier root to have a fixed height, so each individual root
                // must be less than MAX_HEIGHT
                assert!(
                    last_root.height < MAX_HEIGHT,
                    "Height must be less than {MAX_HEIGHT}"
                );
            } else {
                break;
            }
        }

        roots = roots.push(last_root);

        Self {
            roots,
            _hash: std::marker::PhantomData,
        }
    }

    #[must_use]
    pub fn frontier_root(&self) -> Fr {
        let mut root = empty_subtree_root::<Hash>(0);
        let mut height = 0;
        for last in &self.roots {
            while height < last.height - 1 {
                root = Hash::compress(&[root, empty_subtree_root::<Hash>(height as usize)]);
                height += 1;
            }
            root = Hash::compress(&[last.root, root]);
            height += 1;
        }
        assert!(height <= MAX_HEIGHT);
        // ensure a fixed depth
        while height < MAX_HEIGHT {
            root = Hash::compress(&[root, empty_subtree_root::<Hash>(height as usize)]);
            height += 1;
        }

        assert_eq!(height, MAX_HEIGHT);

        root
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.roots
            .iter()
            .map(|r| (1 << (r.height - 1)) as usize)
            .sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

fn empty_subtree_root<Hash: Digest>(height: usize) -> Fr {
    static PRECOMPUTED_EMPTY_ROOTS: OnceLock<[Fr; 32]> = OnceLock::new();
    assert!(height < 32, "Height must be less than 32: {height}");
    PRECOMPUTED_EMPTY_ROOTS.get_or_init(|| {
        let mut hashes = [EMPTY_VALUE; 32];
        for i in 1..32 {
            hashes[i] = Hash::compress(&[hashes[i - 1], hashes[i - 1]]);
        }
        hashes
    })[height]
}

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
        ZkHasher::digest(&[b2p(data)])
    }

    #[test]
    #[expect(clippy::clone_on_copy, reason = "for the sake of the test")]
    fn test_empty_roots() {
        let mut root = Fr::ZERO;
        for i in 0..32 {
            assert_eq!(root, empty_subtree_root::<ZkHasher>(i));
            root = <ZkHasher as Digest>::compress(&[root.clone(), root]);
        }
    }

    fn padded_leaves(elements: impl IntoIterator<Item = impl AsRef<[u8]>>, height: u8) -> Vec<Fr> {
        let mut leaves = elements
            .into_iter()
            .map(|e| leaf(e.as_ref()))
            .collect::<Vec<_>>();
        let pad = (1 << height as usize) - leaves.len();
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
            mmr = mmr.push(elem.as_ref().into());
        }
        assert_eq!(mmr.frontier_root(), root(&padded_leaves(elems, 8)));
    }

    #[ignore = "very slow"]
    #[property_test]
    fn test_frontier_root_16(elems: Vec<[u8; 32]>) {
        let mut mmr = <MerkleMountainRange<TestFr, ZkHasher, 16>>::new();
        for elem in &elems {
            mmr = mmr.push(elem.as_ref().into());
        }
        assert_eq!(mmr.frontier_root(), root(&padded_leaves(elems, 16)));
    }

    #[test]
    fn test_empty_tree() {
        let mmr = <MerkleMountainRange<TestFr, ZkHasher>>::new();
        assert_eq!(mmr.len(), 0);
        assert!(mmr.is_empty());
    }

    #[test]
    fn test_mmr_push() {
        let mut mmr = <MerkleMountainRange<TestFr, ZkHasher>>::new().push(b"hello".as_ref().into());
        assert_eq!(mmr.len(), 1);
        assert_eq!(mmr.roots.size(), 1);
        assert_eq!(mmr.roots.peek().unwrap().height, 1);
        assert_eq!(mmr.roots.peek().unwrap().root, leaf(b"hello"));

        mmr = mmr.push(b"world".as_ref().into());
        assert_eq!(mmr.len(), 2);
        assert_eq!(mmr.roots.size(), 1);
        assert_eq!(mmr.roots.peek().unwrap().height, 2);
        assert_eq!(
            mmr.roots.peek().unwrap().root,
            <ZkHasher as Digest>::compress(&[leaf(b"hello"), leaf(b"world")])
        );

        mmr = mmr.push(b"!".as_ref().into());
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

        mmr = mmr.push(b"!".as_ref().into());
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
    }
}
