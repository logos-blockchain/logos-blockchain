//! Indexed Merkle tree of nullifiers, proving non membership with the low
//! leaf of a nullifier as defined in the Mantle specification.
//!
//! [`store::Imt`] keeps the tree in a [`lb_kv_store::KvStore`], copy on write,
//! so that the states of every fork stay readable and the memory does not grow
//! with the set; see `DESIGN.md`. [`IndexedMerkleTree`] keeps the whole tree in
//! memory and is the reference the storage backed tree is tested against.

mod leaf;
mod memory;
pub mod store;

pub use leaf::{LeafIndex, NullifierLeaf};
pub use memory::{ImtError, IndexedMerkleTree};
