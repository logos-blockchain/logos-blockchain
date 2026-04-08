use core::{hash::Hash, marker::PhantomData};
use std::num::NonZeroU64;

use lb_blend_message::{
    Error, PaddedPayloadBody, PayloadType, crypto::proofs::PoQVerificationInputsMinusSigningKey,
    input::EncapsulationInput,
};
use lb_blend_proofs::quota::inputs::prove::{
    private::ProofOfLeadershipQuotaInputs, public::LeaderInputs,
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::X25519PrivateKey;

use crate::{
    membership::Membership,
    message_blend::{
        crypto::{
            EncapsulatedMessageWithVerifiedPublicHeader, SessionCryptographicProcessorSettings,
        },
        provers::{ProofsGeneratorSettings, core_and_leader::CoreAndLeaderProofsGenerator},
    },
};

/// [`SessionCryptographicProcessor`] is responsible for only wrapping
/// cover and data messages for the message indistinguishability.
///
/// Each instance is meant to be used during a single session.
pub struct SessionCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator> {
    num_blend_layers: NonZeroU64,
    /// The non-ephemeral encryption key (NEK) for decapsulating messages.
    non_ephemeral_encryption_key: X25519PrivateKey,
    membership: Membership<NodeId>,
    session: u64,
    proofs_generator: ProofsGenerator,
    _phantom: PhantomData<CorePoQGenerator>,
}

impl<NodeId, CorePoQGenerator, ProofsGenerator>
    SessionCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>
{
    pub(super) const fn non_ephemeral_encryption_key(&self) -> &X25519PrivateKey {
        &self.non_ephemeral_encryption_key
    }

    pub(super) const fn membership(&self) -> &Membership<NodeId> {
        &self.membership
    }

    pub const fn session(&self) -> u64 {
        self.session
    }

    #[cfg(test)]
    pub const fn proofs_generator(&self) -> &ProofsGenerator {
        &self.proofs_generator
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator>
    SessionCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>
where
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
{
    #[must_use]
    pub fn new(
        settings: SessionCryptographicProcessorSettings,
        membership: Membership<NodeId>,
        public_info: PoQVerificationInputsMinusSigningKey,
        core_proof_of_quota_generator: CorePoQGenerator,
        epoch: Epoch,
    ) -> Self {
        tracing::trace!(
            "Creating session cryptographic processor with public info {public_info:?} and epoch {epoch:?}"
        );

        let generator_settings = ProofsGeneratorSettings {
            local_node_index: membership.local_index(),
            membership_size: membership.size(),
            public_inputs: public_info,
            encapsulation_layers: settings.num_blend_layers,
            epoch,
        };
        Self {
            num_blend_layers: settings.num_blend_layers,
            non_ephemeral_encryption_key: settings.non_ephemeral_encryption_key,
            membership,
            proofs_generator: ProofsGenerator::new(
                generator_settings,
                core_proof_of_quota_generator,
            ),
            session: public_info.session,
            _phantom: PhantomData,
        }
    }

    pub fn rotate_epoch(&mut self, new_epoch_public_info: LeaderInputs, new_epoch: Epoch) {
        tracing::trace!(
            "Rotating epoch with new public info {new_epoch_public_info:?} and new epoch {new_epoch:?}"
        );
        self.proofs_generator
            .rotate_epoch(new_epoch_public_info, new_epoch);
    }

    pub fn set_epoch_private(
        &mut self,
        new_epoch_private: ProofOfLeadershipQuotaInputs,
        new_epoch_public_info: LeaderInputs,
        new_epoch: Epoch,
    ) {
        self.proofs_generator.set_epoch_private(
            new_epoch_private,
            new_epoch_public_info,
            new_epoch,
        );
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator>
    SessionCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>
where
    NodeId: Eq + Hash + 'static,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
{
    pub async fn encapsulate_cover_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        self.encapsulate_payload(PayloadType::Cover, payload).await
    }

    pub async fn encapsulate_data_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        self.encapsulate_payload(PayloadType::Data, payload).await
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

        match payload_type {
            PayloadType::Cover => {
                for _ in 0..self.num_blend_layers.into() {
                    let Some(proof) = self.proofs_generator.get_next_core_proof().await else {
                        return Err(Error::NoMoreProofOfQuotas);
                    };
                    proofs.push(proof);
                }
            }
            PayloadType::Data => {
                for _ in 0..self.num_blend_layers.into() {
                    let Some(proof) = self.proofs_generator.get_next_leader_proof().await else {
                        return Err(Error::NoLeadershipInfoProvided);
                    };
                    proofs.push(proof);
                }
            }
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
            payload_type,
            validated_payload,
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
    use multiaddr::{Multiaddr, PeerId};

    use super::SessionCryptographicProcessor;
    use crate::{
        membership::{Membership, Node},
        message_blend::crypto::{
            SessionCryptographicProcessorSettings,
            test_utils::{MockCorePoQGenerator, TestEpochChangeCoreAndLeaderProofsGenerator},
        },
    };

    #[test]
    fn epoch_rotation() {
        let mut processor = SessionCryptographicProcessor::<
            _,
            _,
            TestEpochChangeCoreAndLeaderProofsGenerator,
        >::new(
            SessionCryptographicProcessorSettings {
                non_ephemeral_encryption_key: [0; _].into(),
                num_blend_layers: NonZeroU64::new(1).unwrap(),
            },
            Membership::new_without_local(&[Node {
                address: Multiaddr::empty(),
                id: PeerId::random(),
                public_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
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
            MockCorePoQGenerator,
            Epoch::new(0),
        );

        let new_leader_inputs = LeaderInputs {
            pol_ledger_aged: ZkHash::ONE,
            pol_epoch_nonce: ZkHash::ONE,
            message_quota: 2,
            lottery_0: Fr::ONE,
            lottery_1: Fr::ONE,
        };

        processor.rotate_epoch(new_leader_inputs, Epoch::new(1));

        assert_eq!(
            processor.proofs_generator.0.public_inputs.leader,
            new_leader_inputs
        );
    }

    #[test]
    fn set_epoch_private() {
        let leader_inputs = LeaderInputs {
            message_quota: 1,
            pol_epoch_nonce: ZkHash::ZERO,
            pol_ledger_aged: ZkHash::ZERO,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
        };
        let mut processor = SessionCryptographicProcessor::<
            _,
            _,
            TestEpochChangeCoreAndLeaderProofsGenerator,
        >::new(
            SessionCryptographicProcessorSettings {
                non_ephemeral_encryption_key: [0; _].into(),
                num_blend_layers: NonZeroU64::new(1).unwrap(),
            },
            Membership::new_without_local(&[Node {
                address: Multiaddr::empty(),
                id: PeerId::random(),
                public_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
            }]),
            PoQVerificationInputsMinusSigningKey {
                session: 1,
                core: CoreInputs {
                    quota: 1,
                    zk_root: ZkHash::ZERO,
                },
                leader: leader_inputs,
            },
            MockCorePoQGenerator,
            Epoch::new(0),
        );

        let new_private_inputs = ProofOfLeadershipQuotaInputs {
            aged_path_and_selectors: [(ZkHash::ONE, true); _],
            note_value: 2,
            output_number: 2,
            slot: 2,
            secret_key: ZkHash::ONE,
            transaction_hash: ZkHash::ONE,
        };

        processor.set_epoch_private(new_private_inputs.clone(), leader_inputs, Epoch::new(1));

        assert!(processor.proofs_generator.1 == Some(new_private_inputs));
    }
}
