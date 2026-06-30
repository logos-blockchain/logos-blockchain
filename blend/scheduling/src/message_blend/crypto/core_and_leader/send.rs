use core::{hash::Hash, marker::PhantomData};
use std::num::NonZeroU64;

use lb_blend_message::{
    Error, PaddedPayloadBody, PayloadType, crypto::proofs::PoQVerificationInputsMinusSigningKey,
    input::EncapsulationInput,
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::keys::X25519PrivateKey;

use crate::{
    membership::Membership,
    message_blend::{
        crypto::{
            EncapsulatedMessageWithVerifiedPublicHeader, EpochCryptographicProcessorSettings,
        },
        provers::{
            ProofsGeneratorSettings, WinningPolInfoStream,
            core_and_leader::CoreAndLeaderProofsGenerator,
        },
    },
};

/// [`EpochCryptographicProcessor`] is responsible for only wrapping
/// cover and data messages for the message indistinguishability.
///
/// Each instance is meant to be used during a single epoch.
pub struct EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator> {
    num_blend_layers: NonZeroU64,
    /// The non-ephemeral encryption key (NEK) for decapsulating messages.
    non_ephemeral_encryption_key: X25519PrivateKey,
    membership: Membership<NodeId>,
    proofs_generator: ProofsGenerator,
    _phantom: PhantomData<CorePoQGenerator>,
}

impl<NodeId, CorePoQGenerator, ProofsGenerator>
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>
{
    pub(super) const fn non_ephemeral_encryption_key(&self) -> &X25519PrivateKey {
        &self.non_ephemeral_encryption_key
    }

    pub(super) const fn membership(&self) -> &Membership<NodeId> {
        &self.membership
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
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
{
    #[must_use]
    pub fn new(
        settings: EpochCryptographicProcessorSettings,
        membership: Membership<NodeId>,
        public_info: PoQVerificationInputsMinusSigningKey,
        core_proof_of_quota_generator: CorePoQGenerator,
        epoch: Epoch,
    ) -> Self {
        tracing::trace!(
            "Creating epoch cryptographic processor with public info {public_info:?} and epoch {epoch:?}"
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
                        return Err(Error::ProofNotAvailable);
                    };
                    proofs.push(proof);
                }
            }
            PayloadType::Data => {
                for _ in 0..self.num_blend_layers.into() {
                    let Some(proof) = self.proofs_generator.get_next_leader_proof().await else {
                        return Err(Error::ProofNotAvailable);
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
        )
        .expect("Number of encapsulation layers is greater than 0."))
    }
}

#[cfg(test)]
mod test {
    use std::num::NonZeroU64;

    use futures::{StreamExt as _, stream::repeat};
    use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
    use lb_blend_proofs::quota::inputs::prove::{
        private::ProofOfLeadershipQuotaInputs,
        public::{CoreInputs, LeaderInputs},
    };
    use lb_core::crypto::ZkHash;
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Field as _, Fr};
    use lb_key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};
    use libp2p::PeerId;
    use multiaddr::Multiaddr;

    use super::EpochCryptographicProcessor;
    use crate::{
        membership::{Membership, Node},
        message_blend::crypto::{
            EpochCryptographicProcessorSettings,
            test_utils::{MockCorePoQGenerator, TestEpochChangeCoreAndLeaderProofsGenerator},
        },
    };

    #[tokio::test]
    async fn set_epoch_private() {
        let leader_inputs = LeaderInputs {
            message_quota: 1,
            pol_epoch_nonce: ZkHash::ZERO,
            pol_ledger_aged: ZkHash::ZERO,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
        };
        let mut processor =
            EpochCryptographicProcessor::<_, _, TestEpochChangeCoreAndLeaderProofsGenerator>::new(
                EpochCryptographicProcessorSettings {
                    non_ephemeral_encryption_key: [0; _].into(),
                    num_blend_layers: NonZeroU64::new(1).unwrap(),
                },
                Membership::new_without_local(&[Node {
                    address: Multiaddr::empty(),
                    id: PeerId::random(),
                    public_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE])
                        .unwrap(),
                }]),
                PoQVerificationInputsMinusSigningKey {
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

        processor.set_epoch_private(Box::pin(repeat(new_private_inputs.clone())), Epoch::new(1));

        // The generator now stores the winning-slot stream; pulling its first item
        // yields the inputs we provided.
        let first_slot = processor.proofs_generator.0.as_mut().unwrap().next().await;
        assert!(first_slot == Some(new_private_inputs));
    }
}
