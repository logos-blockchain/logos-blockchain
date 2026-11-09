//! The keys and values the tree stores, and their byte encodings.
//!
//! Integers are big-endian so that the byte order of the keys is their
//! numeric order, and a [`Version`] comes first wherever a whole version is
//! deleted by prefix.

use lb_groth16::{fr_from_bytes, fr_to_bytes};
use lb_poseidon2::Fr;

use crate::{LeafIndex, NullifierLeaf};

/// The version of the nodes a committed block wrote, unique among the blocks
/// a store ever held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64);

impl Version {
    /// The version of the genesis tree.
    pub const GENESIS: Self = Self(0);

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// The id of a block, as the ledger names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub [u8; 32]);

/// A node position: level 0 holds the leaves, level `HEIGHT` the root, and the
/// children of `(level, index)` are `(level - 1, 2 index)` and
/// `(level - 1, 2 index + 1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodePosition {
    pub level: u8,
    pub index: u64,
}

impl NodePosition {
    #[must_use]
    pub const fn leaf(index: LeafIndex) -> Self {
        Self {
            level: 0,
            index: index.as_u64(),
        }
    }

    #[must_use]
    pub const fn root(height: usize) -> Self {
        Self {
            level: height as u8,
            index: 0,
        }
    }

    #[must_use]
    pub const fn parent(self) -> Self {
        Self {
            level: self.level + 1,
            index: self.index >> 1,
        }
    }

    #[must_use]
    pub const fn left(self) -> Self {
        Self {
            level: self.level - 1,
            index: self.index << 1,
        }
    }

    #[must_use]
    pub const fn right(self) -> Self {
        Self {
            level: self.level - 1,
            index: (self.index << 1) | 1,
        }
    }

    #[must_use]
    pub const fn is_right_child(self) -> bool {
        self.index & 1 == 1
    }

    /// The child of `self` on the way down to `target`, a position below it.
    #[must_use]
    pub const fn child_towards(self, target: Self) -> Self {
        let shift = self.level - 1 - target.level;
        if (target.index >> shift) & 1 == 1 {
            self.right()
        } else {
            self.left()
        }
    }
}

/// A stored node: the node at `position` as written by the block of
/// `version`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeKey {
    pub version: Version,
    pub position: NodePosition,
}

/// Records that the block of `replaced_by` replaced `node`, which can be
/// deleted once no retained state is older than that block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StaleKey {
    pub replaced_by: Version,
    pub node: NodeKey,
}

/// A finalized block whose replaced nodes wait for every retained state to
/// reach it: the tree had `end_len` leaves after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GcKey {
    pub end_len: u64,
    pub version: Version,
}

/// A finalized nullifier, to its leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NullifierKey {
    pub nullifier: Fr,
}

/// A block, to the version of its writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockVersionKey {
    pub block_id: BlockId,
}

/// A version not final yet, to its block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LiveVersionKey {
    pub version: Version,
}

/// The finalized base of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetaKey;

/// The next version to allocate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NextVersionKey;

/// A child of a node: its hash and the version of its stored node, `None` for
/// an empty subtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Child {
    pub hash: Fr,
    pub version: Option<Version>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredNode {
    Internal { left: Child, right: Child },
    Leaf { leaf: NullifierLeaf },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub leaf_index: LeafIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveVersion {
    pub block_id: BlockId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaleMarker;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcMarker;

/// The finalized tree: its size, its root and the block it is the state of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseMeta {
    pub len: u64,
    pub root: Child,
    pub block_id: BlockId,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("{0} bytes missing")]
    Truncated(usize),
    #[error("{0} trailing bytes")]
    Trailing(usize),
    #[error("tag {found} where {expected} was expected")]
    Tag { expected: u8, found: u8 },
    #[error("{0} is not a field element")]
    InvalidFr(String),
}

pub trait StoreKey: Sized {
    const TAG: u8;

    fn write_fields(&self, out: &mut Vec<u8>);

    fn read_fields(reader: &mut Reader<'_>) -> Result<Self, CodecError>;

    #[must_use]
    fn encode(&self) -> Vec<u8> {
        let mut out = vec![Self::TAG];
        self.write_fields(&mut out);
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut reader = Reader::new(bytes);
        let found = reader.u8()?;
        if found != Self::TAG {
            return Err(CodecError::Tag {
                expected: Self::TAG,
                found,
            });
        }
        let key = Self::read_fields(&mut reader)?;
        reader.finish()?;
        Ok(key)
    }
}

pub trait StoreValue: Sized {
    fn write(&self, out: &mut Vec<u8>);

    fn read(reader: &mut Reader<'_>) -> Result<Self, CodecError>;

    #[must_use]
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut reader = Reader::new(bytes);
        let value = Self::read(&mut reader)?;
        reader.finish()?;
        Ok(value)
    }
}

/// Reads fields off encoded bytes.
pub struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    const fn take<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        let Some((head, rest)) = self.bytes.split_first_chunk::<N>() else {
            return Err(CodecError::Truncated(N - self.bytes.len()));
        };
        self.bytes = rest;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take::<1>()?[0])
    }

    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_be_bytes(self.take()?))
    }

    fn fr(&mut self) -> Result<Fr, CodecError> {
        let mut bytes = self.take::<32>()?;
        bytes.reverse();
        fr_from_bytes(&bytes).map_err(|error| CodecError::InvalidFr(error.to_string()))
    }

    const fn finish(&self) -> Result<(), CodecError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(CodecError::Trailing(self.bytes.len()))
        }
    }
}

/// The 32 big-endian bytes of `fr`, whose byte order is the integer order.
#[must_use]
pub fn fr_be_bytes(fr: &Fr) -> [u8; 32] {
    let mut bytes = fr_to_bytes(fr);
    bytes.reverse();
    bytes
}

fn write_position(position: NodePosition, out: &mut Vec<u8>) {
    out.push(position.level);
    out.extend_from_slice(&position.index.to_be_bytes());
}

fn read_position(reader: &mut Reader<'_>) -> Result<NodePosition, CodecError> {
    Ok(NodePosition {
        level: reader.u8()?,
        index: reader.u64()?,
    })
}

fn write_node_key(key: NodeKey, out: &mut Vec<u8>) {
    out.extend_from_slice(&key.version.0.to_be_bytes());
    write_position(key.position, out);
}

fn read_node_key(reader: &mut Reader<'_>) -> Result<NodeKey, CodecError> {
    Ok(NodeKey {
        version: Version(reader.u64()?),
        position: read_position(reader)?,
    })
}

impl NodeKey {
    /// The prefix of every node the block of `version` wrote.
    #[must_use]
    pub fn version_prefix(version: Version) -> Vec<u8> {
        let mut out = vec![Self::TAG];
        out.extend_from_slice(&version.0.to_be_bytes());
        out
    }
}

impl StaleKey {
    /// The prefix of every node the block of `replaced_by` replaced.
    #[must_use]
    pub fn version_prefix(replaced_by: Version) -> Vec<u8> {
        let mut out = vec![Self::TAG];
        out.extend_from_slice(&replaced_by.0.to_be_bytes());
        out
    }
}

impl GcKey {
    #[must_use]
    pub fn prefix() -> Vec<u8> {
        vec![Self::TAG]
    }
}

impl StoreKey for GcKey {
    const TAG: u8 = b'G';

    fn write_fields(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.end_len.to_be_bytes());
        out.extend_from_slice(&self.version.0.to_be_bytes());
    }

    fn read_fields(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            end_len: reader.u64()?,
            version: Version(reader.u64()?),
        })
    }
}

impl NullifierKey {
    #[must_use]
    pub fn prefix() -> Vec<u8> {
        vec![Self::TAG]
    }
}

impl LiveVersionKey {
    #[must_use]
    pub fn prefix() -> Vec<u8> {
        vec![Self::TAG]
    }
}

impl StoreKey for NodeKey {
    const TAG: u8 = b'H';

    fn write_fields(&self, out: &mut Vec<u8>) {
        write_node_key(*self, out);
    }

    fn read_fields(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        read_node_key(reader)
    }
}

impl StoreKey for StaleKey {
    const TAG: u8 = b'S';

    fn write_fields(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.replaced_by.0.to_be_bytes());
        write_node_key(self.node, out);
    }

    fn read_fields(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            replaced_by: Version(reader.u64()?),
            node: read_node_key(reader)?,
        })
    }
}

impl StoreKey for NullifierKey {
    const TAG: u8 = b'N';

    fn write_fields(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&fr_be_bytes(&self.nullifier));
    }

    fn read_fields(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            nullifier: reader.fr()?,
        })
    }
}

impl StoreKey for BlockVersionKey {
    const TAG: u8 = b'B';

    fn write_fields(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.block_id.0);
    }

    fn read_fields(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            block_id: BlockId(reader.take()?),
        })
    }
}

impl StoreKey for LiveVersionKey {
    const TAG: u8 = b'L';

    fn write_fields(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.version.0.to_be_bytes());
    }

    fn read_fields(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            version: Version(reader.u64()?),
        })
    }
}

impl StoreKey for MetaKey {
    const TAG: u8 = b'M';

    fn write_fields(&self, _out: &mut Vec<u8>) {}

    fn read_fields(_reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self)
    }
}

impl StoreKey for NextVersionKey {
    const TAG: u8 = b'C';

    fn write_fields(&self, _out: &mut Vec<u8>) {}

    fn read_fields(_reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self)
    }
}

impl StoreValue for Version {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.to_be_bytes());
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self(reader.u64()?))
    }
}

const NO_VERSION: u8 = 0;
const SOME_VERSION: u8 = 1;

impl StoreValue for Child {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&fr_be_bytes(&self.hash));
        match self.version {
            None => out.push(NO_VERSION),
            Some(version) => {
                out.push(SOME_VERSION);
                version.write(out);
            }
        }
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        let hash = reader.fr()?;
        let version = match reader.u8()? {
            NO_VERSION => None,
            SOME_VERSION => Some(Version::read(reader)?),
            found => {
                return Err(CodecError::Tag {
                    expected: SOME_VERSION,
                    found,
                });
            }
        };
        Ok(Self { hash, version })
    }
}

impl StoreValue for NullifierLeaf {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&fr_be_bytes(&self.nullifier));
        out.extend_from_slice(&fr_be_bytes(&self.next_nullifier));
        out.extend_from_slice(&self.next_index.as_u64().to_be_bytes());
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            nullifier: reader.fr()?,
            next_nullifier: reader.fr()?,
            next_index: LeafIndex::new(reader.u64()?),
        })
    }
}

const INTERNAL_NODE: u8 = 0;
const LEAF_NODE: u8 = 1;

impl StoreValue for StoredNode {
    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::Internal { left, right } => {
                out.push(INTERNAL_NODE);
                left.write(out);
                right.write(out);
            }
            Self::Leaf { leaf } => {
                out.push(LEAF_NODE);
                leaf.write(out);
            }
        }
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        match reader.u8()? {
            INTERNAL_NODE => Ok(Self::Internal {
                left: Child::read(reader)?,
                right: Child::read(reader)?,
            }),
            LEAF_NODE => Ok(Self::Leaf {
                leaf: NullifierLeaf::read(reader)?,
            }),
            found => Err(CodecError::Tag {
                expected: LEAF_NODE,
                found,
            }),
        }
    }
}

impl StoreValue for IndexEntry {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.leaf_index.as_u64().to_be_bytes());
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            leaf_index: LeafIndex::new(reader.u64()?),
        })
    }
}

impl StoreValue for LiveVersion {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.block_id.0);
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            block_id: BlockId(reader.take()?),
        })
    }
}

impl StoreValue for GcMarker {
    fn write(&self, _out: &mut Vec<u8>) {}

    fn read(_reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self)
    }
}

impl StoreValue for StaleMarker {
    fn write(&self, _out: &mut Vec<u8>) {}

    fn read(_reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self)
    }
}

impl StoreValue for BaseMeta {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.len.to_be_bytes());
        self.root.write(out);
        out.extend_from_slice(&self.block_id.0);
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            len: reader.u64()?,
            root: Child::read(reader)?,
            block_id: BlockId(reader.take()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use ark_ff::Field as _;
    use num_bigint::BigUint;

    use super::*;

    fn roundtrip_key<K: StoreKey + PartialEq + std::fmt::Debug>(key: &K) {
        assert_eq!(&K::decode(&key.encode()).unwrap(), key);
    }

    fn roundtrip_value<V: StoreValue + PartialEq + std::fmt::Debug>(value: &V) {
        assert_eq!(&V::decode(&value.encode()).unwrap(), value);
    }

    #[test]
    fn test_keys_roundtrip() {
        let node = NodeKey {
            version: Version(7),
            position: NodePosition {
                level: 3,
                index: 1 << 33,
            },
        };
        roundtrip_key(&node);
        roundtrip_key(&StaleKey {
            replaced_by: Version(9),
            node,
        });
        roundtrip_key(&NullifierKey {
            nullifier: -Fr::ONE,
        });
        roundtrip_key(&BlockVersionKey {
            block_id: BlockId([3; 32]),
        });
        roundtrip_key(&LiveVersionKey {
            version: Version(u64::MAX),
        });
        roundtrip_key(&GcKey {
            end_len: 12,
            version: Version(4),
        });
        roundtrip_key(&MetaKey);
        roundtrip_key(&NextVersionKey);
    }

    #[test]
    fn test_values_roundtrip() {
        let child = Child {
            hash: Fr::from(5),
            version: Some(Version(2)),
        };
        let empty = Child {
            hash: Fr::from(6),
            version: None,
        };
        roundtrip_value(&Version(42));
        roundtrip_value(&child);
        roundtrip_value(&empty);
        roundtrip_value(&StoredNode::Internal {
            left: child,
            right: empty,
        });
        roundtrip_value(&StoredNode::Leaf {
            leaf: NullifierLeaf {
                nullifier: -Fr::ONE,
                next_nullifier: Fr::from(1),
                next_index: LeafIndex::new(9),
            },
        });
        roundtrip_value(&IndexEntry {
            leaf_index: LeafIndex::new(11),
        });
        roundtrip_value(&LiveVersion {
            block_id: BlockId([1; 32]),
        });
        roundtrip_value(&StaleMarker);
        roundtrip_value(&GcMarker);
        roundtrip_value(&BaseMeta {
            len: 3,
            root: child,
            block_id: BlockId([8; 32]),
        });
    }

    #[test]
    fn test_decoding_rejects_bad_bytes() {
        let encoded = NodeKey {
            version: Version(1),
            position: NodePosition { level: 0, index: 0 },
        }
        .encode();
        assert!(matches!(
            StaleKey::decode(&encoded),
            Err(CodecError::Tag { .. })
        ));
        assert!(matches!(
            NodeKey::decode(&encoded[..encoded.len() - 1]),
            Err(CodecError::Truncated(1))
        ));
        let mut long = encoded;
        long.push(0);
        assert!(matches!(
            NodeKey::decode(&long),
            Err(CodecError::Trailing(1))
        ));
        // the modulus is not a field element
        assert!(matches!(
            NullifierKey::decode(&[b"N".as_slice(), &[0xff; 32]].concat()),
            Err(CodecError::InvalidFr(_))
        ));
    }

    #[test]
    fn test_nullifier_key_order_is_integer_order() {
        let mut nullifiers = vec![
            Fr::from(0),
            Fr::from(1),
            Fr::from(255),
            Fr::from(256),
            Fr::from(BigUint::from(1u8) << 200),
            -Fr::from(2),
            -Fr::ONE,
        ];
        let sorted = nullifiers.clone();
        nullifiers.reverse();
        let mut keys: Vec<Vec<u8>> = nullifiers
            .iter()
            .map(|nullifier| {
                NullifierKey {
                    nullifier: *nullifier,
                }
                .encode()
            })
            .collect();
        keys.sort();
        let decoded: Vec<Fr> = keys
            .iter()
            .map(|key| NullifierKey::decode(key).unwrap().nullifier)
            .collect();
        assert_eq!(decoded, sorted);
        assert!(sorted.is_sorted());
    }

    #[test]
    fn test_position_navigation() {
        let root = NodePosition::root(4);
        let leaf = NodePosition::leaf(LeafIndex::new(0b1011));
        let mut position = root;
        for _ in 0..4 {
            position = position.child_towards(leaf);
        }
        assert_eq!(position, leaf);
        let mut position = leaf;
        for _ in 0..4 {
            position = position.parent();
        }
        assert_eq!(position, root);
        assert!(leaf.is_right_child());
        assert_eq!(
            leaf.parent().left(),
            NodePosition::leaf(LeafIndex::new(0b1010))
        );
    }
}
