use blake2::digest::typenum::U32;
pub type Hasher = blake2::Blake2b<U32>;
pub use blake2::digest::Digest;
pub type Hash = [u8; 32];

pub type ZkHasher = lb_poseidon2::Poseidon2Bn254Hasher;
pub use lb_poseidon2::{Digest as ZkDigest, ZkHash};
