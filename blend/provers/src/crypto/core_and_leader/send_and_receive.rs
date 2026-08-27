use core::ops::{Deref, DerefMut};

use lb_blend_membership::Membership;
use lb_blend_message::{
    Error,
    crypto::proofs::PoQVerificationInputsMinusSigningKey,
    encap::{ProofsVerifier as ProofsVerifierTrait, decapsulated::DecapsulationOutput},
};
use lb_cryptarchia_engine::Epoch;

use crate::{
    crypto::{
        EncapsulatedMessageWithVerifiedPublicHeader, EpochCryptographicProcessorSettings,
        core_and_leader::{
            receive::EpochCryptographicProcessor as ReceiverEpochCryptographicProcessor,
            send::EpochCryptographicProcessor as SenderEpochCryptographicProcessor,
        },
    },
    provers::core_leader_and_pow::CoreLeaderAndPowProofsGenerator,
};

/// [`EpochCryptographicProcessor`] is responsible for wrapping both cover and
/// data messages and unwrapping messages for the message indistinguishability.
///
/// Each instance is meant to be used during a single epoch.
///
/// This processor is suitable for core nodes.
pub struct EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier> {
    sender_processor: SenderEpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>,
    receiver_processor: ReceiverEpochCryptographicProcessor<ProofsVerifier>,
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
where
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
{
    #[must_use]
    pub fn new(
        settings: EpochCryptographicProcessorSettings,
        membership: Membership<NodeId>,
        public_info: PoQVerificationInputsMinusSigningKey,
        core_proof_of_quota_generator: CorePoQGenerator,
        epoch: Epoch,
    ) -> Self {
        let EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key,
            num_blend_layers,
            pow_mining_pool,
            spent_core_quota,
        } = settings;
        Self {
            receiver_processor: ReceiverEpochCryptographicProcessor::new(
                non_ephemeral_encryption_key,
                &membership,
                public_info,
                epoch,
            ),
            sender_processor: SenderEpochCryptographicProcessor::new(
                num_blend_layers,
                membership,
                public_info,
                core_proof_of_quota_generator,
                epoch,
                pow_mining_pool,
                spent_core_quota,
            ),
        }
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
{
    pub const fn verifier(&self) -> &ProofsVerifier {
        self.receiver_processor.verifier()
    }

    pub const fn epoch(&self) -> Epoch {
        self.receiver_processor.epoch()
    }

    pub const fn receiver(&self) -> &ReceiverEpochCryptographicProcessor<ProofsVerifier> {
        &self.receiver_processor
    }

    /// Give up the send side of this processor, keeping only what it takes to
    /// decapsulate.
    #[must_use]
    pub fn into_receiver_only(self) -> ReceiverEpochCryptographicProcessor<ProofsVerifier> {
        self.receiver_processor
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait,
{
    pub fn decapsulate_message(
        &self,
        message: EncapsulatedMessageWithVerifiedPublicHeader,
    ) -> Result<DecapsulationOutput, Error> {
        self.receiver_processor.decapsulate_message(message)
    }
}

// `Deref` and `DerefMut` so we can call the `encapsulate*` methods exposed by
// the send-only processor.
impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier> Deref
    for EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
{
    type Target = SenderEpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>;

    fn deref(&self) -> &Self::Target {
        &self.sender_processor
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier> DerefMut
    for EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sender_processor
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
    use multiaddr::{Multiaddr, PeerId};
    use rayon::ThreadPoolBuilder;

    use crate::crypto::{
        EpochCryptographicProcessorSettings,
        core_and_leader::send_and_receive::EpochCryptographicProcessor,
        test_utils::{
            MockCorePoQGenerator, TestEpochChangeCoreAndLeaderProofsGenerator,
            TestEpochChangeProofsVerifier,
        },
    };

    /// `set_epoch_private` propagates private inputs for leader proof
    /// generation.
    #[tokio::test]
    async fn set_epoch_private_updates_generator() {
        let initial_leader = LeaderInputs {
            message_quota: Quota::ONE,
            pol_epoch_nonce: ZkHash::ZERO,
            pol_ledger_aged: ZkHash::ZERO,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
        };
        let mut processor = EpochCryptographicProcessor::<
            _,
            _,
            TestEpochChangeCoreAndLeaderProofsGenerator,
            TestEpochChangeProofsVerifier,
        >::new(
            EpochCryptographicProcessorSettings {
                non_ephemeral_encryption_key: [0; _].into(),
                num_blend_layers: NonZeroU64::new(1).unwrap(),
                pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
                spent_core_quota: Quota::ZERO,
            },
            Membership::new_without_local(&[Node {
                address: Multiaddr::empty(),
                id: PeerId::random(),
                public_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
            }]),
            PoQVerificationInputsMinusSigningKey {
                core: CoreInputs {
                    quota: Quota::ONE,
                    zk_root: ZkHash::ZERO,
                },
                leader: initial_leader,
                pow: PowInputs::disabled(),
            },
            MockCorePoQGenerator,
            Epoch::new(0),
        );

        assert!(processor.proofs_generator().0.is_none());

        let private_inputs = ProofOfLeadershipQuotaInputs {
            aged_path_and_selectors: [(ZkHash::ONE, true); _],
            note_value: 42,
            output_number: 1,
            slot: 1,
            secret_key: ZkHash::ONE,
            transaction_hash: ZkHash::ONE,
        };

        processor.set_epoch_private(Box::pin(repeat(private_inputs.clone())), Epoch::new(1));

        // The generator now stores the winning-slot stream; pulling its first item
        // yields the inputs we provided.
        let first_slot = processor
            .proofs_generator_mut()
            .0
            .as_mut()
            .unwrap()
            .next()
            .await;
        assert!(first_slot == Some(private_inputs));
    }
}
