mod hasher;
pub use ark_bn254::Fr;
use jf_poseidon2::Poseidon2;

pub type Poseidon2Bn254 = Poseidon2<Fr>;
pub type Poseidon2Bn254Hasher = hasher::Poseidon2Hasher;
pub type ZkHash = Fr;

pub trait Digest {
    /// Digest takes `inputs` data and 1st apply the 10* padding protocol and
    /// apply Poseidon2 hash function.
    ///
    /// This can be used for anything.
    fn digest(inputs: &[Fr]) -> ZkHash;

    /// Compress takes exactly 2 elements as `inputs`. It doesn't apply the
    /// padding and apply the Poseidon2 hash function.
    ///
    /// This can only be used for protocols where the inputs are always of size
    /// 2. In Logos blockchain it's reserved for Merkle proofs, public key
    ///    derivation,
    /// nullifier derivation and reward voucher derivation.
    fn compress(inputs: &[Fr; 2]) -> ZkHash;

    fn new() -> Self;
    fn update(&mut self, input: &Fr);
    fn finalize(self) -> ZkHash;
}
