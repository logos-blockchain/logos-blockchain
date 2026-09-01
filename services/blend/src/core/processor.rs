use std::ops::{Deref, DerefMut};

pub use lb_blend::scheduling::message_blend::crypto::core_and_leader::receive::EpochCryptographicProcessor as ReceiverCryptographicProcessor;
use lb_blend::{
    message::{
        crypto::proofs::PoQVerificationInputsMinusSigningKey,
        encap::ProofsVerifier as ProofsVerifierTrait,
    },
    scheduling::{
        membership::Membership,
        message_blend::{
            crypto::{
                EpochCryptographicProcessorSettings,
                core_and_leader::send_and_receive::EpochCryptographicProcessor,
            },
            provers::core_leader_and_pow::CoreLeaderAndPowProofsGenerator,
        },
    },
};
use lb_chain_service::Epoch;

pub struct CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>(
    EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
);

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
    CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
{
    pub const fn epoch(&self) -> Epoch {
        self.0.epoch()
    }

    /// Retire this processor into the read-only one an epoch that has ended is
    /// left with.
    #[must_use]
    pub fn rotate_epoch(self) -> ReceiverCryptographicProcessor<ProofsVerifier> {
        self.0.into_receiver_only()
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
    CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
where
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
{
    pub fn new(
        membership: Membership<NodeId>,
        settings: EpochCryptographicProcessorSettings,
        public_info: PoQVerificationInputsMinusSigningKey,
        core_proof_of_quota_generator: CorePoQGenerator,
        epoch: Epoch,
    ) -> Self {
        Self(EpochCryptographicProcessor::new(
            settings,
            membership,
            public_info,
            core_proof_of_quota_generator,
            epoch,
        ))
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier> Deref
    for CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
{
    type Target =
        EpochCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier> DerefMut
    for CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;
    use std::sync::Arc;

    use lb_blend::{
        message::{
            Error as InnerError, PayloadType,
            crypto::{
                key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey,
            },
            encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
            input::EncapsulationInput,
        },
        proofs::{
            quota::{
                VerifiedProofOfQuota,
                inputs::prove::public::{CoreInputs, LeaderInputs, PowInputs},
            },
            selection::{self, VerifiedProofOfSelection},
        },
        scheduling::message_blend::crypto::{
            EpochCryptographicProcessorSettings, core_and_leader::receive::DecapsulatedMessageType,
        },
    };
    use lb_chain_service::Epoch;
    use lb_core::crypto::ZkHash;
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
    use lb_poq::Quota;
    use rayon::ThreadPoolBuilder;

    use crate::{
        core::processor::CoreCryptographicProcessor,
        test_utils::{
            crypto::{MockCoreAndLeaderProofsGenerator, StaticFetchVerifier},
            membership::{key, membership},
        },
    };

    fn mock_verification_inputs() -> PoQVerificationInputsMinusSigningKey {
        use lb_groth16::AdditiveGroup as _;

        PoQVerificationInputsMinusSigningKey {
            core: CoreInputs {
                quota: Quota::ONE,
                zk_root: ZkHash::ZERO,
            },
            leader: LeaderInputs {
                pol_ledger_aged: ZkHash::ZERO,
                pol_epoch_nonce: ZkHash::ZERO,
                message_quota: Quota::ONE,
                lottery_0: Fr::ZERO,
                lottery_1: Fr::ZERO,
            },
            pow: PowInputs::disabled(),
        }
    }

    #[test]
    fn decapsulate_recursive_top_level_failure() {
        let local_id = NodeId(1);
        let membership = membership(&[local_id], local_id);
        let mock_message = {
            let node_key = &membership
                .get_node_at(membership.local_index().unwrap())
                .unwrap()
                .public_key;
            mock_message(node_key)
        };
        let processor = CoreCryptographicProcessor::<
            _,
            _,
            MockCoreAndLeaderProofsGenerator,
            StaticFetchVerifier,
        >::new(
            membership,
            settings(local_id),
            mock_verification_inputs(),
            (),
            Epoch::new(0),
        );
        assert!(matches!(
            processor
                .receiver()
                .decapsulate_message_recursive(mock_message),
            Err(InnerError::ProofOfSelectionVerificationFailed(
                selection::Error::Verification
            ))
        ));
    }

    #[test]
    fn decapsulate_recursive_one_layer() {
        let local_id = NodeId(1);
        let membership = membership(&[local_id], local_id);
        let mock_message = {
            let node_key = &membership
                .get_node_at(membership.local_index().unwrap())
                .unwrap()
                .public_key;
            mock_message(node_key)
        };
        let processor = CoreCryptographicProcessor::<
            _,
            _,
            MockCoreAndLeaderProofsGenerator,
            StaticFetchVerifier,
        >::new(
            membership,
            settings(local_id),
            mock_verification_inputs(),
            (),
            Epoch::new(0),
        );
        StaticFetchVerifier::set_remaining_valid_poq_proofs(1);
        let decapsulation_output = processor
            .receiver()
            .decapsulate_message_recursive(mock_message)
            .unwrap();
        let (blending_tokens, remaining_message_type) = decapsulation_output.into_components();
        assert_eq!(blending_tokens.len(), 1);
        assert!(matches!(
            remaining_message_type,
            DecapsulatedMessageType::Incompleted(_)
        ));
    }

    #[test]
    fn decapsulate_recursive_two_layers() {
        let local_id = NodeId(1);
        let membership = membership(&[local_id], local_id);
        let mock_message = {
            let node_key = &membership
                .get_node_at(membership.local_index().unwrap())
                .unwrap()
                .public_key;
            mock_message(node_key)
        };
        let processor = CoreCryptographicProcessor::<
            _,
            _,
            MockCoreAndLeaderProofsGenerator,
            StaticFetchVerifier,
        >::new(
            membership,
            settings(local_id),
            mock_verification_inputs(),
            (),
            Epoch::new(0),
        );
        StaticFetchVerifier::set_remaining_valid_poq_proofs(2);
        let decapsulation_output = processor
            .receiver()
            .decapsulate_message_recursive(mock_message)
            .unwrap();
        let (blending_tokens, remaining_message_type) = decapsulation_output.into_components();
        assert_eq!(blending_tokens.len(), 2);
        assert!(matches!(
            remaining_message_type,
            DecapsulatedMessageType::Incompleted(_)
        ));
    }

    #[test]
    fn decapsulate_recursive_all_layers() {
        let local_id = NodeId(1);
        let membership = membership(&[local_id], local_id);
        let mock_message = {
            let node_key = &membership
                .get_node_at(membership.local_index().unwrap())
                .unwrap()
                .public_key;
            mock_message(node_key)
        };
        let processor = CoreCryptographicProcessor::<
            _,
            _,
            MockCoreAndLeaderProofsGenerator,
            StaticFetchVerifier,
        >::new(
            membership,
            settings(local_id),
            mock_verification_inputs(),
            (),
            Epoch::new(0),
        );
        StaticFetchVerifier::set_remaining_valid_poq_proofs(3);
        let decapsulation_output = processor
            .receiver()
            .decapsulate_message_recursive(mock_message)
            .unwrap();
        let (blending_tokens, remaining_message_type) = decapsulation_output.into_components();
        assert_eq!(blending_tokens.len(), 3);
        assert!(matches!(
            remaining_message_type,
            DecapsulatedMessageType::Completed(_)
        ));
    }

    fn mock_message(
        recipient_signing_pubkey: &Ed25519PublicKey,
    ) -> EncapsulatedMessageWithVerifiedPublicHeader {
        let inputs = std::iter::repeat_with(|| {
            EncapsulationInput::try_new(
                UnsecuredEd25519Key::generate_with_chacha_rng(),
                recipient_signing_pubkey,
                VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
                VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            )
            .unwrap()
        })
        .take(3)
        .collect::<Vec<_>>();
        EncapsulatedMessageWithVerifiedPublicHeader::try_new(
            &inputs,
            PayloadType::Cover,
            b"".as_slice().try_into().unwrap(),
            3,
        )
        .unwrap()
    }

    fn settings(local_id: NodeId) -> EpochCryptographicProcessorSettings {
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: key(local_id).0.derive_x25519(),
            num_blend_layers: NonZeroU64::new(1).unwrap(),
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    struct NodeId(u8);

    impl From<NodeId> for [u8; 32] {
        fn from(id: NodeId) -> Self {
            [id.0; 32]
        }
    }
}
