use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
use lb_blend_proofs::{
    quota::{VerifiedProofOfQuota, inputs::prove::public::CoreInputs},
    selection::VerifiedProofOfSelection,
};
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;

pub mod core;
pub mod core_and_leader;
pub mod leader;

#[cfg(test)]
mod test_utils;

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
}

#[derive(Debug, Clone, Copy)]
pub struct NewCoreSessionPublicInputs {
    pub session: u64,
    pub local_node_index: usize,
    pub membership_size: usize,
    pub inputs: CoreInputs,
}
