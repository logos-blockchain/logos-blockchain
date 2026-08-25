use core::{future::ready, num::NonZeroU64};

use futures::{Stream, StreamExt as _};
use lb_blend_proofs::quota::{self, KeyIndex, VerifiedProofOfQuota, inputs::prove::PublicInputs};
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
        key_index: KeyIndex,
    ) -> impl Future<Output = Result<(VerifiedProofOfQuota, ZkHash), quota::Error>> + Send + Sync;
}

/// Groups a stream of individual layer proofs into whole-message sets.
pub(crate) fn into_encapsulation_sets(
    proofs_stream: impl Stream<Item = provers::BlendLayerProof> + Send,
    encapsulation_layers: NonZeroU64,
) -> impl Stream<Item = provers::EncapsulationProofs> + Send {
    proofs_stream
        .chunks(encapsulation_layers.get() as usize)
        .filter_map(move |run| {
            ready(provers::EncapsulationProofs::try_new(run, encapsulation_layers).ok())
        })
}

const fn buffer_size(encapsulation_layers: usize) -> usize {
    // We need to keep "warm" the first proof of the next cycle, so that when the
    // stream is polled the first time for a new set of proofs, all proofs, from
    // first to last, are ready.
    encapsulation_layers + 1
}
