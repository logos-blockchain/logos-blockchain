use core::ops::{Deref, DerefMut};

use lb_blend_message::{
    Error,
    crypto::proofs::PoQVerificationInputsMinusSigningKey,
    encap::{
        ProofsVerifier as ProofsVerifierTrait, decapsulated::DecapsulationOutput,
        validated::RequiredProofOfSelectionVerificationInputs,
    },
};
use lb_cryptarchia_engine::Epoch;

use crate::{
    membership::Membership,
    message_blend::{
        crypto::{
            EncapsulatedMessageWithVerifiedPublicHeader, EpochCryptographicProcessorSettings,
            core_and_leader::send::EpochCryptographicProcessor as SenderEpochCryptographicProcessor,
        },
        provers::core_and_leader::CoreAndLeaderProofsGenerator,
    },
};

/// [`EpochCryptographicProcessor`] is responsible for wrapping both cover and
/// data messages and unwrapping messages for the message indistinguishability.
///
/// Each instance is meant to be used during a single epoch.
///
/// This processor is suitable for core nodes.
pub struct EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier> {
    sender_processor: SenderEpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator>,
    proofs_verifier: ProofsVerifier,
    epoch: Epoch,
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
where
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
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
        Self {
            sender_processor: SenderEpochCryptographicProcessor::new(
                settings,
                membership,
                public_info,
                core_proof_of_quota_generator,
                epoch,
            ),
            proofs_verifier: ProofsVerifier::new(public_info),
            epoch,
        }
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
{
    pub const fn verifier(&self) -> &ProofsVerifier {
        &self.proofs_verifier
    }

    pub const fn epoch(&self) -> Epoch {
        self.epoch
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
        let Some(local_node_index) = self.sender_processor.membership().local_index() else {
            return Err(Error::NotCoreNodeReceiver);
        };
        message.decapsulate(
            self.sender_processor.non_ephemeral_encryption_key(),
            &RequiredProofOfSelectionVerificationInputs {
                expected_node_index: local_node_index as u64,
                total_membership_size: self.sender_processor.membership().size() as u64,
            },
            &self.proofs_verifier,
        )
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
    use std::num::NonZeroU64;

    use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
    use lb_blend_proofs::quota::inputs::prove::{
        private::ProofOfLeadershipQuotaInputs,
        public::{CoreInputs, LeaderInputs},
    };
    use lb_core::crypto::ZkHash;
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Field as _, Fr};
    use lb_key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};
    use multiaddr::{Multiaddr, PeerId};

    use crate::{
        membership::{Membership, Node},
        message_blend::crypto::{
            EpochCryptographicProcessorSettings,
            core_and_leader::send_and_receive::EpochCryptographicProcessor,
            test_utils::{
                MockCorePoQGenerator, TestEpochChangeCoreAndLeaderProofsGenerator,
                TestEpochChangeProofsVerifier,
            },
        },
    };

    /// `set_epoch_private` propagates private inputs for leader proof
    /// generation.
    #[test]
    fn set_epoch_private_updates_generator() {
        let initial_leader = LeaderInputs {
            message_quota: 1,
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
            },
            Membership::new_without_local(&[Node {
                address: Multiaddr::empty(),
                id: PeerId::random(),
                public_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
            }]),
            PoQVerificationInputsMinusSigningKey {
                core: CoreInputs {
                    quota: 1,
                    zk_root: ZkHash::ZERO,
                },
                leader: initial_leader,
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

        processor.set_epoch_private(private_inputs.clone(), Epoch::new(1));

        assert!(processor.proofs_generator().0 == Some(private_inputs));
    }
}
