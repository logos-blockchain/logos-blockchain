use core::hash::Hash;
use std::num::NonZeroU64;

use lb_blend_message::{
    Error, PaddedPayloadBody, PayloadType, crypto::proofs::PoQVerificationInputsMinusSigningKey,
    input::EncapsulationInput,
};
use lb_blend_proofs::quota::inputs::prove::{
    private::ProofOfLeadershipQuotaInputs, public::LeaderInputs,
};
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

/// [`SessionCryptographicProcessor`] is responsible for only wrapping data
/// messages (no cover messages) for the message indistinguishability.
///
/// Each instance is meant to be used during a single session.
///
/// This processor is suitable for non-core nodes that do not need to generate
/// any cover traffic and are hence only interested in blending data messages.
pub struct SessionCryptographicProcessor<NodeId, ProofsGenerator> {
    num_blend_layers: NonZeroU64,
    membership: Membership<NodeId>,
    proofs_generator: ProofsGenerator,
}

impl<NodeId, ProofsGenerator> SessionCryptographicProcessor<NodeId, ProofsGenerator>
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
        }
    }

    pub fn rotate_epoch(
        &mut self,
        new_epoch_public: LeaderInputs,
        new_private_inputs: ProofOfLeadershipQuotaInputs,
        new_epoch: Epoch,
    ) {
        self.proofs_generator
            .rotate_epoch(new_epoch_public, new_private_inputs, new_epoch);
    }
}

impl<NodeId, ProofsGenerator> SessionCryptographicProcessor<NodeId, ProofsGenerator>
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
                tracing::trace!("Encapsulating layer {layer:?} of data message for node at index {node_index:?}.");
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
                EncapsulationInput::new(
                    proof.ephemeral_signing_key,
                    &receiver_non_ephemeral_signing_key,
                    proof.proof_of_quota,
                    proof.proof_of_selection,
                )
            })
            .collect::<Vec<_>>();

        Ok(EncapsulatedMessageWithVerifiedPublicHeader::new(
            &inputs,
            PayloadType::Data,
            validated_payload,
        ))
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

#[cfg(test)]
mod test {
    use std::num::NonZeroU64;

    use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
    use lb_blend_proofs::quota::inputs::prove::{
        private::ProofOfLeadershipQuotaInputs,
        public::{CoreInputs, LeaderInputs},
    };
    use lb_core::crypto::ZkHash;
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{Field as _, Fr};
    use lb_key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};
    use libp2p::{Multiaddr, PeerId};

    use super::SessionCryptographicProcessor;
    use crate::{
        membership::{Membership, Node},
        message_blend::crypto::test_utils::TestEpochChangeLeaderProofsGenerator,
    };

    #[test]
    fn epoch_rotation() {
        let mut processor =
            SessionCryptographicProcessor::<_, TestEpochChangeLeaderProofsGenerator>::new(
                NonZeroU64::new(1).unwrap(),
                Membership::new_without_local(&[Node {
                    address: Multiaddr::empty(),
                    id: PeerId::random(),
                    public_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE])
                        .unwrap(),
                }]),
                PoQVerificationInputsMinusSigningKey {
                    session: 1,
                    core: CoreInputs {
                        quota: 1,
                        zk_root: ZkHash::ZERO,
                    },
                    leader: LeaderInputs {
                        message_quota: 1,
                        pol_epoch_nonce: ZkHash::ZERO,
                        pol_ledger_aged: ZkHash::ZERO,
                        lottery_0: Fr::ZERO,
                        lottery_1: Fr::ZERO,
                    },
                },
                ProofOfLeadershipQuotaInputs {
                    aged_path_and_selectors: [(ZkHash::ZERO, false); _],
                    note_value: 1,
                    output_number: 1,
                    secret_key: ZkHash::ZERO,
                    slot: 1,
                    transaction_hash: ZkHash::ZERO,
                },
                Epoch::new(0),
            );

        let new_leader_inputs = LeaderInputs {
            pol_ledger_aged: ZkHash::ONE,
            pol_epoch_nonce: ZkHash::ONE,
            message_quota: 2,
            lottery_0: Fr::ONE,
            lottery_1: Fr::ONE,
        };
        let new_private_inputs = ProofOfLeadershipQuotaInputs {
            aged_path_and_selectors: [(ZkHash::ONE, true); _],
            note_value: 2,
            output_number: 2,
            secret_key: ZkHash::ONE,
            slot: 2,
            transaction_hash: ZkHash::ONE,
        };

        processor.rotate_epoch(new_leader_inputs, new_private_inputs.clone(), Epoch::new(1));

        assert_eq!(
            processor.proofs_generator.0.public_inputs.leader,
            new_leader_inputs
        );
        assert!(processor.proofs_generator.1 == new_private_inputs);
    }
}
