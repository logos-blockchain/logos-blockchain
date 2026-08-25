use core::hash::Hash;
use std::{num::NonZeroU64, sync::Arc};

use lb_blend_message::{
    Error, PaddedPayloadBody, crypto::proofs::PoQVerificationInputsMinusSigningKey,
    input::EncapsulationInput,
};
use lb_cryptarchia_engine::Epoch;
use rayon::ThreadPool;

use crate::{
    membership::Membership,
    message_blend::{
        crypto::EncapsulatedMessageWithVerifiedPublicHeader,
        provers::{
            BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
            leader_and_pow::LeaderAndPowProofsGenerator,
        },
    },
};

#[derive(Debug, Clone, Copy)]
enum PayloadType {
    BlockProposal,
    Transaction,
}

impl From<PayloadType> for lb_blend_message::PayloadType {
    fn from(value: PayloadType) -> Self {
        match value {
            PayloadType::BlockProposal => Self::BlockProposal,
            PayloadType::Transaction => Self::Transaction,
        }
    }
}

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
    ProofsGenerator: LeaderAndPowProofsGenerator,
{
    #[must_use]
    pub fn new(
        num_blend_layers: NonZeroU64,
        membership: Membership<NodeId>,
        public_info: PoQVerificationInputsMinusSigningKey,
        winning_pol_info_stream: WinningPolInfoStream,
        epoch: Epoch,
        pow_mining_pool: Arc<ThreadPool>,
    ) -> Self {
        let generator_settings = ProofsGeneratorSettings {
            local_node_index: membership.local_index(),
            membership_size: membership.size(),
            public_inputs: public_info,
            encapsulation_layers: num_blend_layers,
            epoch,
            pow_mining_pool,
        };
        Self {
            num_blend_layers,
            membership,
            proofs_generator: ProofsGenerator::new(generator_settings, winning_pol_info_stream),
            epoch,
        }
    }
}

impl<NodeId, ProofsGenerator> EpochCryptographicProcessor<NodeId, ProofsGenerator>
where
    NodeId: Eq + Hash + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator,
{
    pub async fn encapsulate_block_proposal_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        self.encapsulate_payload(PayloadType::BlockProposal, payload)
            .await
    }

    pub async fn encapsulate_transaction_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        self.encapsulate_payload(PayloadType::Transaction, payload)
            .await
    }

    async fn encapsulate_payload(
        &mut self,
        payload_type: PayloadType,
        payload: &[u8],
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        // We validate the payload early on so we don't generate proofs unnecessarily.
        let validated_payload = PaddedPayloadBody::try_from(payload)?;
        let mut proofs = Vec::with_capacity(self.num_blend_layers.get() as usize);

        for _ in 0..self.num_blend_layers.into() {
            let Some(proof) = self.next_proof_for(payload_type).await else {
                return Err(Error::ProofNotAvailable);
            };
            proofs.push(proof);
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
                    "Encapsulating layer {layer:?} of data message type {payload_type:?} for node at index {node_index:?}."
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
            payload_type.into(),
            validated_payload,
            self.num_blend_layers.get() as usize,
        )
        .expect("Number of encapsulation inputs is in `1..=num_blend_layers`."))
    }

    /// The `PoQ` branch each payload type draws its layer proofs from.
    ///
    /// An edge node has no core quota, so unlike a core node it has nothing to
    /// spend on cover traffic — and it generates none.
    async fn next_proof_for(&mut self, payload_type: PayloadType) -> Option<BlendLayerProof> {
        match payload_type {
            PayloadType::BlockProposal => self.proofs_generator.get_next_leader_proof().await,
            PayloadType::Transaction => self.proofs_generator.get_next_pow_proof().await,
        }
    }
}
