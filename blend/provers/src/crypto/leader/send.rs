use core::{hash::Hash, mem};
use std::{num::NonZeroU64, sync::Arc};

use lb_blend_membership::Membership;
use lb_blend_message::{
    Error, MAX_PAYLOAD_BODY_SIZE, PaddedPayloadBody,
    crypto::proofs::PoQVerificationInputsMinusSigningKey, input::EncapsulationInput,
};
use lb_cryptarchia_engine::Epoch;
use lb_log_targets::blend;
use rayon::ThreadPool;

use crate::{
    crypto::EncapsulatedMessageWithVerifiedPublicHeader,
    provers::{
        BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
        leader_and_pow::LeaderAndPowProofsGenerator,
    },
};

const LOG_TARGET: &str = blend::processor::leader::SEND;

#[derive(Debug, Clone, Copy)]
pub(crate) enum PayloadType {
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

/// Layer proofs drawn for a message that was never finished. See the core
/// processor's `PartialDraws` for why they are worth keeping.
#[derive(Default)]
pub(crate) struct PartialDraws {
    block_proposal: Vec<BlendLayerProof>,
    transaction: Vec<BlendLayerProof>,
}

impl PartialDraws {
    pub const fn for_type(&mut self, payload_type: PayloadType) -> &mut Vec<BlendLayerProof> {
        match payload_type {
            PayloadType::BlockProposal => &mut self.block_proposal,
            PayloadType::Transaction => &mut self.transaction,
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
    partial_draws: PartialDraws,
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
            partial_draws: PartialDraws::default(),
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
        // Refuse a payload that could never fit before spending anything on it.
        // Only the length check has to happen this early; padding it — an 18 KiB
        // allocation with a random tail — waits until the proofs are in hand, so
        // an attempt that comes up short or is cancelled costs nothing.
        if payload.len() > MAX_PAYLOAD_BODY_SIZE {
            return Err(Error::PayloadTooLarge);
        }

        let Some(proofs) = self.next_proofs_for(payload_type).await else {
            return Err(Error::ProofNotAvailable);
        };

        Ok(self.encapsulate_with(payload_type, PaddedPayloadBody::try_from(payload)?, proofs))
    }

    /// Draws a whole message's layer proofs, resuming any run a previous
    /// attempt left unfinished.
    ///
    /// Proofs are accumulated on `self` rather than in a local variable, so a
    /// caller that is cancelled mid-draw leaves them where the next attempt
    /// will find them.
    ///
    /// A branch that runs out part-way does not sink the message: the wire
    /// format carries `ß_max` blending headers whatever happens, so a message
    /// can go out under fewer real layers. Returns `None` only when not one
    /// proof is available.
    async fn next_proofs_for(&mut self, payload_type: PayloadType) -> Option<Vec<BlendLayerProof>> {
        let encapsulations = self.num_blend_layers.get() as usize;
        while self.partial_draws.for_type(payload_type).len() < encapsulations
            && let Some(layer_proof) = self.next_proof_for(payload_type).await
        {
            self.partial_draws.for_type(payload_type).push(layer_proof);
        }

        let message_proofs = mem::take(self.partial_draws.for_type(payload_type));
        if message_proofs.is_empty() {
            return None;
        }

        if message_proofs.len() < encapsulations {
            tracing::warn!(
                target: LOG_TARGET,
                "Encapsulating a {payload_type:?} message under {} of {encapsulations} layers: its quota branch is exhausted for this epoch.",
                message_proofs.len()
            );
        }
        Some(message_proofs)
    }

    fn encapsulate_with(
        &self,
        payload_type: PayloadType,
        validated_payload: PaddedPayloadBody,
        proofs: Vec<BlendLayerProof>,
    ) -> EncapsulatedMessageWithVerifiedPublicHeader {
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
                    target: LOG_TARGET,
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

        EncapsulatedMessageWithVerifiedPublicHeader::try_new(
            &inputs,
            payload_type.into(),
            validated_payload,
            self.num_blend_layers.get() as usize,
        )
        .expect("Number of encapsulation inputs is in `1..=num_blend_layers`.")
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
