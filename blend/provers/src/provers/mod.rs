use ::core::{num::NonZeroU64, pin::Pin};
use futures::Stream;
use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
use lb_blend_proofs::{
    quota::{Quota, VerifiedProofOfQuota, inputs::prove::private::ProofOfLeadershipQuotaInputs},
    selection::VerifiedProofOfSelection,
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::UnsecuredEd25519Key;

pub mod core;
pub mod core_and_leader;
pub mod core_leader_and_pow;
pub mod leader;
pub mod leader_and_pow;
pub mod pow;

#[cfg(test)]
mod test_utils;

/// A stream of winning-slot leadership inputs, one item per winning slot.
///
/// The leadership proof generator pulls a fresh winning slot from this stream
/// for each new data message (advancing the slot every `message_quota` proofs),
/// so each message gets a distinct key nullifier. Backpressure on the
/// underlying channel keeps the producer from materializing the whole epoch.
pub type WinningPolInfoStream = Pin<Box<dyn Stream<Item = ProofOfLeadershipQuotaInputs> + Send>>;

/// A single proof to be attached to one layer of a Blend message.
pub struct BlendLayerProof {
    /// `PoQ`
    pub proof_of_quota: VerifiedProofOfQuota,
    /// `PoSel`
    pub proof_of_selection: VerifiedProofOfSelection,
    /// Ephemeral key used to sign the message layer's payload.
    pub ephemeral_signing_key: UnsecuredEd25519Key,
}

/// Every proof needed to encapsulate one message: one per blend layer.
pub struct EncapsulationProofs(Box<[BlendLayerProof]>);

impl EncapsulationProofs {
    /// Builds a set, rejecting one that could not encapsulate a message.
    ///
    /// # Errors
    ///
    /// [`ProofCountMismatch`] if `layers` does not hold exactly
    /// `encapsulation_layers` proofs.
    pub fn try_new(
        layers: Vec<BlendLayerProof>,
        encapsulation_layers: NonZeroU64,
    ) -> Result<Self, ProofCountMismatch> {
        let expected = encapsulation_layers.get() as usize;
        if layers.len() == expected {
            Ok(Self(layers.into_boxed_slice()))
        } else {
            Err(ProofCountMismatch {
                expected,
                got: layers.len(),
            })
        }
    }

    /// The proofs, outermost layer first.
    #[must_use]
    pub fn into_layers(self) -> impl ExactSizeIterator<Item = BlendLayerProof> {
        self.0.into_vec().into_iter()
    }

    #[must_use]
    pub const fn layers(&self) -> &[BlendLayerProof] {
        &self.0
    }

    #[must_use]
    pub const fn outermost(&self) -> &BlendLayerProof {
        self.0.first().unwrap()
    }

    #[must_use]
    pub const fn innermost(&self) -> &BlendLayerProof {
        self.0.last().unwrap()
    }

    /// The one proof in a set built for a single-layer epoch.
    ///
    /// # Panics
    ///
    /// If the set holds anything other than one proof. Only for tests that
    /// configure `encapsulation_layers = 1`, where the distinction between a
    /// message and a proof collapses.
    #[cfg(test)]
    #[must_use]
    pub fn into_single_layer(self) -> BlendLayerProof {
        assert_eq!(
            self.0.len(),
            1,
            "into_single_layer is only meaningful for a single-layer epoch"
        );
        self.0.into_vec().pop().unwrap()
    }
}

/// A proof set that does not match the epoch's encapsulation depth.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("Expected {expected} layer proofs to encapsulate a message, got {got}.")]
pub struct ProofCountMismatch {
    pub expected: usize,
    pub got: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ProofsGeneratorSettings {
    pub local_node_index: Option<usize>,
    pub membership_size: usize,
    pub public_inputs: PoQVerificationInputsMinusSigningKey,
    pub encapsulation_layers: NonZeroU64,
    pub epoch: Epoch,
}

/// What one message costs the quota it is drawn from, in key indices.
///
/// One per layer: a message carries `encapsulation_layers` proofs and each is
/// minted against its own index.
#[must_use]
pub fn message_cost(encapsulation_layers: NonZeroU64) -> Quota {
    Quota::try_new(encapsulation_layers.get())
        .expect("Number of blend layers must fit within the `PoQ` quota width.")
}
