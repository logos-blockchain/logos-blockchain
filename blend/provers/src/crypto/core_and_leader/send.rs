use core::{hash::Hash, marker::PhantomData};
use std::{num::NonZeroU64, sync::Arc};

use lb_blend_membership::Membership;
use lb_blend_message::{
    Error, PaddedPayloadBody, PayloadType, crypto::proofs::PoQVerificationInputsMinusSigningKey,
    input::EncapsulationInput,
};
use lb_blend_proofs::quota::Quota;
use lb_cryptarchia_engine::Epoch;
use lb_groth16::fr_to_bytes;
use rayon::ThreadPool;

use crate::{
    crypto::EncapsulatedMessageWithVerifiedPublicHeader,
    provers::{
        BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
        core_leader_and_pow::CoreLeaderAndPowProofsGenerator,
    },
};

/// [`EpochCryptographicProcessor`] is responsible for only wrapping
/// cover and data messages for the message indistinguishability.
///
/// Each instance is meant to be used during a single epoch.
pub struct EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator> {
    num_blend_layers: NonZeroU64,
    membership: Membership<NodeId>,
    proofs_generator: ProofsGenerator,
    _phantom: PhantomData<CorePoQGenerator>,
}

impl<NodeId, CorePoQGenerator, ProofsGenerator>
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>
{
    /// `ß_max`: how many layer proofs one encapsulation draws from the
    /// generator, and therefore how much quota it spends.
    pub const fn num_blend_layers(&self) -> NonZeroU64 {
        self.num_blend_layers
    }

    #[cfg(test)]
    pub const fn proofs_generator(&self) -> &ProofsGenerator {
        &self.proofs_generator
    }

    #[cfg(test)]
    pub const fn proofs_generator_mut(&mut self) -> &mut ProofsGenerator {
        &mut self.proofs_generator
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator>
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>
where
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
{
    #[must_use]
    pub fn new(
        encapsulation_layers: NonZeroU64,
        membership: Membership<NodeId>,
        public_info: PoQVerificationInputsMinusSigningKey,
        core_proof_of_quota_generator: CorePoQGenerator,
        epoch: Epoch,
        pow_mining_pool: Arc<ThreadPool>,
        spent_core_quota: Quota,
    ) -> Self {
        tracing::trace!(
            "Creating epoch cryptographic processor with public info {public_info:?} and epoch {epoch:?}, resuming core key indices from {spent_core_quota}"
        );

        let generator_settings = ProofsGeneratorSettings {
            local_node_index: membership.local_index(),
            membership_size: membership.size(),
            public_inputs: public_info,
            encapsulation_layers,
            epoch,
            pow_mining_pool,
        };
        Self {
            num_blend_layers: encapsulation_layers,
            membership,
            proofs_generator: ProofsGenerator::new(
                generator_settings,
                // Spent core quota == starting key index
                spent_core_quota,
                core_proof_of_quota_generator,
            ),
            _phantom: PhantomData,
        }
    }

    pub fn set_epoch_private(
        &mut self,
        winning_pol_info_stream: WinningPolInfoStream,
        target_epoch: Epoch,
    ) {
        self.proofs_generator
            .set_epoch_private(winning_pol_info_stream, target_epoch);
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator>
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>
where
    NodeId: Eq + Hash + 'static,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
{
    pub async fn encapsulate_cover_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        self.encapsulate_payload(PayloadType::Cover, payload).await
    }

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

    // TODO: Think about optimizing this by, e.g., using less encapsulations if
    // there are less than 3 proofs available, or use a proof from a different pool
    // if needed (core proof for leadership message or leadership proof for
    // cover message, since the protocol does not enforce that).
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
            // Map retrieved indices to the nodes' public keys.
            .enumerate()
            .inspect(|(layer, (proof, node_index))| {
                tracing::trace!("Encapsulating layer {layer:?} of message type {payload_type:?} for node at index {node_index:?} with proof with public key and key nullifier: ({:?}, {:?}). Local node index: {:?}", proof.ephemeral_signing_key.public_key(), hex::encode(fr_to_bytes(&proof.proof_of_quota.key_nullifier())), self.membership.local_index());
            })
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
            payload_type,
            validated_payload,
            self.num_blend_layers.get() as usize,
        )
        .expect("Number of encapsulation inputs is in `1..=num_blend_layers`."))
    }

    /// The `PoQ` branch each payload type draws its layer proofs from.
    async fn next_proof_for(&mut self, payload_type: PayloadType) -> Option<BlendLayerProof> {
        match payload_type {
            PayloadType::Cover => self.proofs_generator.get_next_core_proof().await,
            PayloadType::BlockProposal => self.proofs_generator.get_next_leader_proof().await,
            PayloadType::Transaction => self.proofs_generator.get_next_pow_proof().await,
        }
    }
}

#[cfg(test)]
mod test {
    use std::{num::NonZeroU64, sync::Arc};

    use futures::{StreamExt as _, stream::repeat};
    use lb_blend_membership::{Membership, Node};
    use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
    use lb_blend_proofs::quota::{
        Quota,
        inputs::prove::{
            private::ProofOfLeadershipQuotaInputs,
            public::{CoreInputs, LeaderInputs, PowInputs},
        },
    };
    use lb_core::crypto::ZkHash;
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Field as _, Fr};
    use lb_key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};
    use libp2p::PeerId;
    use multiaddr::Multiaddr;
    use rayon::ThreadPoolBuilder;

    use super::EpochCryptographicProcessor;
    use crate::crypto::test_utils::{
        MockCorePoQGenerator, TestEpochChangeCoreAndLeaderProofsGenerator,
    };

    #[tokio::test]
    async fn set_epoch_private() {
        let leader_inputs = LeaderInputs {
            message_quota: Quota::ONE,
            pol_epoch_nonce: ZkHash::ZERO,
            pol_ledger_aged: ZkHash::ZERO,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
        };
        let mut processor =
            EpochCryptographicProcessor::<_, _, TestEpochChangeCoreAndLeaderProofsGenerator>::new(
                NonZeroU64::new(1).unwrap(),
                Membership::new_without_local(&[Node {
                    address: Multiaddr::empty(),
                    id: PeerId::random(),
                    public_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE])
                        .unwrap(),
                }]),
                PoQVerificationInputsMinusSigningKey {
                    core: CoreInputs {
                        quota: Quota::ONE,
                        zk_root: ZkHash::ZERO,
                    },
                    leader: leader_inputs,
                    pow: PowInputs::disabled(),
                },
                MockCorePoQGenerator,
                Epoch::new(0),
                Arc::new(ThreadPoolBuilder::new().build().unwrap()),
                Quota::ZERO,
            );

        let new_private_inputs = ProofOfLeadershipQuotaInputs {
            aged_path_and_selectors: [(ZkHash::ONE, true); _],
            note_value: 2,
            output_number: 2,
            slot: 2,
            secret_key: ZkHash::ONE,
            transaction_hash: ZkHash::ONE,
        };

        processor.set_epoch_private(Box::pin(repeat(new_private_inputs.clone())), Epoch::new(1));

        // The generator now stores the winning-slot stream; pulling its first item
        // yields the inputs we provided.
        let first_slot = processor.proofs_generator.0.as_mut().unwrap().next().await;
        assert!(first_slot == Some(new_private_inputs));
    }
}
