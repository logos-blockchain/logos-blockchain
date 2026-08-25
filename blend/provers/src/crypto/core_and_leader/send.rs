use core::{hash::Hash, marker::PhantomData, mem};
use std::{num::NonZeroU64, sync::Arc};

use lb_blend_membership::Membership;
use lb_blend_message::{
    Error, MAX_PAYLOAD_BODY_SIZE, PaddedPayloadBody, PayloadType,
    crypto::proofs::PoQVerificationInputsMinusSigningKey, input::EncapsulationInput,
};
use lb_blend_proofs::quota::Quota;
use lb_cryptarchia_engine::Epoch;
use lb_groth16::fr_to_bytes;
use lb_log_targets::blend;
use rayon::ThreadPool;

use crate::{
    crypto::EncapsulatedMessageWithVerifiedPublicHeader,
    provers::{
        BlendLayerProof, ProofsGeneratorSettings, WinningPolInfoStream,
        core_leader_and_pow::CoreLeaderAndPowProofsGenerator,
    },
};

const LOG_TARGET: &str = blend::processor::core_and_leader::SEND;

/// [`EpochCryptographicProcessor`] is responsible for only wrapping
/// cover and data messages for the message indistinguishability.
///
/// Each instance is meant to be used during a single epoch.
pub struct EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator> {
    num_blend_layers: NonZeroU64,
    membership: Membership<NodeId>,
    proofs_generator: ProofsGenerator,
    partial_draws: PartialDraws,
    _phantom: PhantomData<CorePoQGenerator>,
}

/// Layer proofs drawn for a message that was never finished.
///
/// A draw that stops short — because the quota ran out, or because the caller
/// was cancelled part-way — used to drop what it had. Every one of those proofs
/// had already been paid for, and a core one has spent its key index for the
/// epoch, so its nullifier can never be minted again. Keeping them here lets
/// the next attempt carry on instead.
///
/// Kept per branch so resuming cannot quietly change which quota backs a
/// message. Within a branch the proofs are interchangeable: nothing binds one
/// to a particular payload until it is encapsulated.
#[derive(Default)]
struct PartialDraws {
    leader: crate::crypto::leader::send::PartialDraws,
    cover: Vec<BlendLayerProof>,
}

impl PartialDraws {
    const fn for_type(&mut self, payload_type: PayloadType) -> &mut Vec<BlendLayerProof> {
        match payload_type {
            PayloadType::Cover => &mut self.cover,
            PayloadType::BlockProposal => self
                .leader
                .for_type(crate::crypto::leader::send::PayloadType::BlockProposal),
            PayloadType::Transaction => self
                .leader
                .for_type(crate::crypto::leader::send::PayloadType::Transaction),
        }
    }
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
            partial_draws: PartialDraws::default(),
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
    /// Proofs are accumulated on `self` rather than in a local, so a caller
    /// that is cancelled mid-draw — a `select!` arm losing the race, say —
    /// leaves them where the next attempt will find them.
    ///
    /// A branch that runs out part-way does not sink the message: the wire
    /// format carries `ß_max` blending headers whatever happens, padding the
    /// unused ones with random bytes, so a message can go out under fewer real
    /// layers without telling anyone it did. Returns `None` only when not one
    /// proof is available, which is the one case with nothing to send.
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

        EncapsulatedMessageWithVerifiedPublicHeader::try_new(
            &inputs,
            payload_type,
            validated_payload,
            self.num_blend_layers.get() as usize,
        )
        .expect("Number of encapsulation inputs is in `1..=num_blend_layers`.")
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
    use core::pin::pin;
    use std::{num::NonZeroU64, sync::Arc};

    use futures::{StreamExt as _, poll, stream::repeat};
    use lb_blend_membership::{Membership, Node};
    use lb_blend_message::{PayloadType, crypto::proofs::PoQVerificationInputsMinusSigningKey};
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
    use lb_key_management_system_keys::keys::{
        ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey, UnsecuredEd25519Key,
    };
    use libp2p::PeerId;
    use multiaddr::Multiaddr;
    use rayon::ThreadPoolBuilder;

    use super::EpochCryptographicProcessor;
    use crate::crypto::test_utils::{
        MockCorePoQGenerator, RationedCoreProofsGenerator,
        TestEpochChangeCoreAndLeaderProofsGenerator, exhaust_core_branch, ration_core_proofs,
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

    /// A processor whose core branch is under the test's control.
    fn rationed_processor(
        layers: u64,
    ) -> EpochCryptographicProcessor<PeerId, MockCorePoQGenerator, RationedCoreProofsGenerator>
    {
        EpochCryptographicProcessor::new(
            NonZeroU64::new(layers).unwrap(),
            Membership::new_without_local(&[Node {
                address: Multiaddr::empty(),
                id: PeerId::random(),
                // A real key: an all-zero one decodes but has no usable shared
                // secret, which the encapsulation would reject.
                public_key: UnsecuredEd25519Key::from_bytes(&[7; ED25519_PUBLIC_KEY_SIZE])
                    .public_key(),
            }]),
            PoQVerificationInputsMinusSigningKey {
                core: CoreInputs {
                    quota: Quota::ONE,
                    zk_root: ZkHash::ZERO,
                },
                leader: LeaderInputs {
                    message_quota: Quota::ONE,
                    pol_epoch_nonce: ZkHash::ZERO,
                    pol_ledger_aged: ZkHash::ZERO,
                    lottery_0: Fr::ZERO,
                    lottery_1: Fr::ZERO,
                },
                pow: PowInputs {
                    pow_quota: Quota::ONE,
                    pow_blend_difficulty: Fr::ZERO,
                },
            },
            MockCorePoQGenerator,
            Epoch::new(0),
            Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            Quota::ZERO,
        )
    }

    /// A branch that runs out part-way still gets its message out.
    ///
    /// The wire format carries `ß_max` blending headers whatever happens,
    /// padding the unused ones with random bytes, so a message can go out
    /// under fewer real layers without telling anyone. Failing instead
    /// would also waste the proofs already drawn — and a core proof's key
    /// index is spent for the epoch.
    #[tokio::test]
    async fn a_short_branch_sends_under_fewer_layers() {
        let mut processor = rationed_processor(3);

        ration_core_proofs(2);
        exhaust_core_branch(true);

        assert!(
            processor.encapsulate_cover_payload(&[]).await.is_ok(),
            "two of three layers is still a message worth sending"
        );
    }

    /// One layer is the floor: with no proof at all there is nothing to send.
    #[tokio::test]
    async fn no_proofs_at_all_is_the_one_failure() {
        let mut processor = rationed_processor(3);

        ration_core_proofs(0);
        exhaust_core_branch(true);

        assert!(processor.encapsulate_cover_payload(&[]).await.is_err());
    }

    /// A caller abandoned mid-draw leaves its proofs behind for the next one.
    ///
    /// Each cost a proving, and a core one has spent a key index whose
    /// nullifier can never be minted again this epoch — so a `select!` arm
    /// losing the race must not take them with it.
    #[tokio::test]
    async fn proofs_drawn_before_a_cancellation_are_kept() {
        let mut processor = rationed_processor(3);

        // Two proofs, and then the branch blocks rather than ending.
        ration_core_proofs(2);
        exhaust_core_branch(false);
        {
            let draw = pin!(processor.next_proofs_for(PayloadType::Cover));
            assert!(
                poll!(draw).is_pending(),
                "the draw should be waiting on a third proof"
            );
        }; // dropped here, as `select!` drops a losing arm

        // One more proof, and then the branch is done.
        ration_core_proofs(1);
        exhaust_core_branch(true);

        let drawn = processor
            .next_proofs_for(PayloadType::Cover)
            .await
            .expect("one proof is enough to send something");
        assert_eq!(
            drawn.len(),
            3,
            "the two proofs drawn before the cancellation should have been kept"
        );
    }
}
