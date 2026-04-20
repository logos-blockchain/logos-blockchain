use lb_blend_proofs::quota::{self, VerifiedProofOfQuota, inputs::prove::PublicInputs};
use lb_core::crypto::ZkHash;

pub mod crypto;
pub mod provers;

/// A component responsible for statelessly generating core variant `PoQ`s.
///
/// The trait provides the public context as well as the key index, while it
/// assumes the private info is known to the generator.
pub trait CoreProofOfQuotaGenerator {
    fn generate_poq(
        &self,
        public_inputs: &PublicInputs,
        key_index: u64,
    ) -> impl Future<Output = Result<(VerifiedProofOfQuota, ZkHash), quota::Error>> + Send + Sync;
}

const fn buffer_size(encapsulation_layers: usize) -> usize {
    // We need to keep "warm" the first proof of the next cycle, so that when the
    // stream is polled the first time for a new set of proofs, all proofs, from
    // first to last, are ready.
    encapsulation_layers + 1
}
