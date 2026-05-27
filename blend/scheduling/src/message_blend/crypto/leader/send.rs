use core::hash::Hash;
use std::num::NonZeroU64;

use lb_blend_message::{
    Error, PaddedPayloadBody, PayloadType, crypto::proofs::PoQVerificationInputsMinusSigningKey,
    input::EncapsulationInput,
};
use lb_blend_proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_cryptarchia_engine::Epoch;

use crate::{
    membership::Membership,
    message_blend::{
        crypto::{
            EncapsulatedMessageWithVerifiedPublicHeader,
            serialize_encapsulated_message_with_verified_public_header,
        },
        provers::{ProofsGeneratorSettings, leader::LeaderProofsGenerator},
    },
};

/// [`EpochCryptographicProcessor`] is responsible for only wrapping data
/// messages (no cover messages) for the message indistinguishability.
///
/// Each instance is meant to be used during a single epoch.
///
/// This processor is suitable for non-core nodes that do not need to generate
/// any cover traffic and are hence only interested in blending data messages.
pub struct EpochCryptographicProcessor<NodeId, ProofsGenerator> {
    num_blend_layers: NonZeroU64,
    membership: Membership<NodeId>,
    proofs_generator: ProofsGenerator,
    epoch: Epoch,
}

impl<NodeId, ProofsGenerator> EpochCryptographicProcessor<NodeId, ProofsGenerator> {
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }
}

impl<NodeId, ProofsGenerator> EpochCryptographicProcessor<NodeId, ProofsGenerator>
where
    ProofsGenerator: LeaderProofsGenerator,
{
    #[must_use]
    pub fn new(
        num_blend_layers: NonZeroU64,
        membership: Membership<NodeId>,
        public_info: PoQVerificationInputsMinusSigningKey,
        private_info: ProofOfLeadershipQuotaInputs,
        epoch: Epoch,
    ) -> Self {
        let generator_settings = ProofsGeneratorSettings {
            local_node_index: membership.local_index(),
            membership_size: membership.size(),
            public_inputs: public_info,
            encapsulation_layers: num_blend_layers,
            epoch,
        };
        Self {
            num_blend_layers,
            membership,
            proofs_generator: ProofsGenerator::new(generator_settings, private_info),
            epoch,
        }
    }
}

impl<NodeId, ProofsGenerator> EpochCryptographicProcessor<NodeId, ProofsGenerator>
where
    NodeId: Eq + Hash + 'static,
    ProofsGenerator: LeaderProofsGenerator,
{
    pub async fn encapsulate_data_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        // We validate the payload early on so we don't generate proofs unnecessarily.
        let validated_payload = PaddedPayloadBody::try_from(payload)?;
        let mut proofs = Vec::with_capacity(self.num_blend_layers.get() as usize);

        for _ in 0..self.num_blend_layers.into() {
            proofs.push(self.proofs_generator.get_next_proof().await);
        }

        let membership_size = self.membership.size();
        let proofs_and_signing_keys = proofs
            .into_iter()
            // Collect remote (or local) index info for each PoSel.
            .map(|proof| {
                let expected_index = proof
                    .proof_of_selection
                    .expected_index(membership_size)
                    .expect("Node index should exist.");
                (proof, expected_index)
            })
            .enumerate()
            .inspect(|(layer, (_, node_index))| {
                tracing::trace!(
                    "Encapsulating layer {layer:?} of data message for node at index {node_index:?}."
                );
            })
            // Map retrieved indices to the nodes' public keys.
            .map(|(_, (proof, node_index))| {
                (
                    proof,
                    self.membership
                        .get_node_at(node_index)
                        .expect("Node at index should exist.")
                        .public_key,
                )
            });

        let inputs = proofs_and_signing_keys
            .into_iter()
            .map(|(proof, receiver_non_ephemeral_signing_key)| {
                EncapsulationInput::try_new(
                    proof.ephemeral_signing_key,
                    &receiver_non_ephemeral_signing_key,
                    proof.proof_of_quota,
                    proof.proof_of_selection,
                )
                .expect("Layer proof signing key assumed not to be identity")
            })
            .collect::<Vec<_>>();

        Ok(EncapsulatedMessageWithVerifiedPublicHeader::try_new(
            &inputs,
            PayloadType::Data,
            validated_payload,
        )
        .expect("Number of encapsulation layers is greater than 0."))
    }

    pub async fn encapsulate_and_serialize_data_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<Vec<u8>, Error> {
        Ok(serialize_encapsulated_message_with_verified_public_header(
            &self.encapsulate_data_payload(payload).await?,
        ))
    }
}
