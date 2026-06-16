use ::core::{num::NonZeroU64, pin::Pin};
use futures::Stream;
use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
use lb_blend_proofs::{
    quota::{VerifiedProofOfQuota, inputs::prove::private::ProofOfLeadershipQuotaInputs},
    selection::VerifiedProofOfSelection,
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;

pub mod core;
pub mod core_and_leader;
pub mod leader;

#[cfg(test)]
mod test_utils;

/// A stream of winning-slot leadership inputs, one item per winning slot.
///
/// The leadership proof generator pulls a fresh winning slot from this stream
/// for each new data message (advancing the slot every `message_quota` proofs),
/// so each message gets a distinct key nullifier. Backpressure on the
/// underlying channel keeps the producer from materializing the whole epoch.
pub type WinningPolInfoStream =
    Pin<Box<dyn Stream<Item = ProofOfLeadershipQuotaInputs> + Send + Sync>>;

/// A single proof to be attached to one layer of a Blend message.
pub struct BlendLayerProof {
    /// `PoQ`
    pub proof_of_quota: VerifiedProofOfQuota,
    /// `PoSel`
    pub proof_of_selection: VerifiedProofOfSelection,
    /// Ephemeral key used to sign the message layer's payload.
    pub ephemeral_signing_key: UnsecuredEd25519Key,
}

#[derive(Debug, Clone, Copy)]
pub struct ProofsGeneratorSettings {
    pub local_node_index: Option<usize>,
    pub membership_size: usize,
    pub public_inputs: PoQVerificationInputsMinusSigningKey,
    pub encapsulation_layers: NonZeroU64,
    pub epoch: Epoch,
}
