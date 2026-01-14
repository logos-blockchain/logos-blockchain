use std::{
    hash::Hash,
    num::NonZeroU64,
    ops::{Deref, DerefMut},
};

use nomos_blend::{
    message::{
        Error as InnerError,
        crypto::proofs::PoQVerificationInputsMinusSigningKey,
        encap::{
            ProofsVerifier as ProofsVerifierTrait,
            decapsulated::{DecapsulatedMessage, DecapsulationOutput},
            encapsulated::EncapsulatedMessage,
            validated::EncapsulatedMessageWithVerifiedPublicHeader,
        },
        reward::BlendingToken,
    },
    scheduling::{
        membership::Membership,
        message_blend::{
            crypto::{
                SessionCryptographicProcessorSettings,
                core_and_leader::send_and_receive::SessionCryptographicProcessor,
            },
            provers::core_and_leader::CoreAndLeaderProofsGenerator,
        },
    },
};

pub struct CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>(
    SessionCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
);

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
    CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
where
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
{
    pub fn try_new_with_core_condition_check(
        membership: Membership<NodeId>,
        minimum_network_size: NonZeroU64,
        settings: &SessionCryptographicProcessorSettings,
        public_info: PoQVerificationInputsMinusSigningKey,
        core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Result<Self, Error>
    where
        NodeId: Eq + Hash,
    {
        if membership.size() < minimum_network_size.get() as usize {
            Err(Error::NetworkIsTooSmall(membership.size()))
        } else if !membership.contains_local() {
            Err(Error::LocalIsNotCoreNode)
        } else {
            Ok(Self::new(
                membership,
                settings,
                public_info,
                core_proof_of_quota_generator,
            ))
        }
    }

    fn new(
        membership: Membership<NodeId>,
        settings: &SessionCryptographicProcessorSettings,
        public_info: PoQVerificationInputsMinusSigningKey,
        core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self(SessionCryptographicProcessor::new(
            settings,
            membership,
            public_info,
            core_proof_of_quota_generator,
        ))
    }
}

/// The output of a multi-layer decapsulation operation.
#[derive(Debug)]
pub struct MultiLayerDecapsulationOutput {
    /// The blending token collected on the way, one per decapsulated layer.
    blending_tokens: Vec<BlendingToken>,
    /// The final message type.
    decapsulated_message: DecapsulatedMessageType,
}

impl MultiLayerDecapsulationOutput {
    pub fn into_components(self) -> (Vec<BlendingToken>, DecapsulatedMessageType) {
        (self.blending_tokens, self.decapsulated_message)
    }
}

/// The final message type of a multi-layer decapsulation operation.
#[derive(Debug)]
pub enum DecapsulatedMessageType {
    /// The remainder of the message still needs to be decapsulated by some
    /// other node.
    Incompleted(Box<EncapsulatedMessage>),
    /// The message was fully decapsulated, as all the remaining encapsulations
    /// were addressed to this node.
    Completed(DecapsulatedMessage),
}

impl From<DecapsulationOutput> for DecapsulatedMessageType {
    fn from(value: DecapsulationOutput) -> Self {
        match value {
            DecapsulationOutput::Completed {
                fully_decapsulated_message,
                ..
            } => Self::Completed(fully_decapsulated_message),
            DecapsulationOutput::Incompleted {
                remaining_encapsulated_message,
                ..
            } => Self::Incompleted(remaining_encapsulated_message),
        }
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
    CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait,
{
    /// Semantically similar to the underlying
    /// [`SessionCryptographicProcessor::decapsulate_message`], but it does not
    /// stop after decapsulating the outermost layer. It stops only when a layer
    /// cannot be decapsulated or when the decapsulation is completed.
    ///
    /// If no layer (`Err`) or at most one layer (`Ok`) can be decapsulated,
    /// this is semantically equivalent to
    /// calling [`SessionCryptographicProcessor::decapsulate_message`].
    ///
    /// If more than a single layer can be decapsulated, then the decapsulation
    /// happens recursively until the first layer that cannot be decapsulated is
    /// found or when there is no more layers to decapsulate. In either case, it
    /// returns the last processed layer, along with the list of blending tokens
    /// collected along the way.
    pub fn decapsulate_message_recursive(
        &self,
        message: EncapsulatedMessageWithVerifiedPublicHeader,
    ) -> Result<MultiLayerDecapsulationOutput, InnerError> {
        let mut decapsulation_output = self.0.decapsulate_message(message)?;

        let mut collected_blending_tokens = Vec::new();

        loop {
            match &decapsulation_output {
                // We reached the end. Collect token and stop.
                DecapsulationOutput::Completed { blending_token, .. } => {
                    collected_blending_tokens.push(blending_token.clone());
                    break;
                }
                // One or more layers to decapsulate. Collect token from current layer and attempt
                // one more decapsulation.
                DecapsulationOutput::Incompleted {
                    remaining_encapsulated_message,
                    blending_token,
                } => {
                    collected_blending_tokens.push(blending_token.clone());
                    // If we find a message with an invalid public header after a successful
                    // decapsulation, we still bubble it up for the scheduler to
                    // schedule it. At the time of release, the message will be
                    // ignored since its public header cannot be verified. This is not the most
                    // efficient way, but it's the less invasive way since by decapsulation we
                    // currently mean decrypting an encrypted Blend header. No additional checks are
                    // performed on the nested public header. The spec simply ignores the message,
                    // and so we do.
                    let Ok(message_with_validated_public_header) = remaining_encapsulated_message
                        .clone()
                        .verify_public_header(self.verifier())
                    else {
                        break;
                    };
                    let Ok(nested_layer_decapsulation_output) = self
                        .0
                        .decapsulate_message(message_with_validated_public_header)
                    else {
                        break;
                    };
                    decapsulation_output = nested_layer_decapsulation_output;
                }
            }
        }

        Ok(MultiLayerDecapsulationOutput {
            blending_tokens: collected_blending_tokens,
            decapsulated_message: decapsulation_output.into(),
        })
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier> Deref
    for CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>
{
    type Target =
        SessionCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>;

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

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Network is too small: {0}")]
    NetworkIsTooSmall(usize),
    #[error("Local node is not a core node")]
    LocalIsNotCoreNode,
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
    use nomos_blend::{
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
                inputs::prove::public::{CoreInputs, LeaderInputs},
            },
            selection::{self, VerifiedProofOfSelection},
        },
        scheduling::message_blend::crypto::SessionCryptographicProcessorSettings,
    };
    use nomos_core::crypto::ZkHash;

    use crate::{
        core::processor::{CoreCryptographicProcessor, DecapsulatedMessageType, Error},
        test_utils::{
            crypto::{MockCoreAndLeaderProofsGenerator, MockProofsVerifier, StaticFetchVerifier},
            membership::{key, membership},
        },
    };

    fn mock_verification_inputs() -> PoQVerificationInputsMinusSigningKey {
        use groth16::Field as _;

        PoQVerificationInputsMinusSigningKey {
            session: 1,
            core: CoreInputs {
                quota: 1,
                zk_root: ZkHash::ZERO,
            },
            leader: LeaderInputs {
                pol_ledger_aged: ZkHash::ZERO,
                pol_epoch_nonce: ZkHash::ZERO,
                message_quota: 1,
                total_stake: 1,
            },
        }
    }

    #[test]
    fn try_new_with_valid_membership() {
        let local_id = NodeId(1);
        let core_nodes = [NodeId(1)];
        CoreCryptographicProcessor::<_, _, MockCoreAndLeaderProofsGenerator, MockProofsVerifier>::try_new_with_core_condition_check(
            membership(&core_nodes, local_id),
            NonZeroU64::new(1).unwrap(),
            &settings(local_id),
            mock_verification_inputs(),
            ()
        )
        .unwrap();
    }

    #[test]
    fn try_new_with_small_membership() {
        let local_id = NodeId(1);
        let core_nodes = [NodeId(1)];
        let result = CoreCryptographicProcessor::<
            _,
            _,
            MockCoreAndLeaderProofsGenerator,
            MockProofsVerifier,
        >::try_new_with_core_condition_check(
            membership(&core_nodes, local_id),
            NonZeroU64::new(2).unwrap(),
            &settings(local_id),
            mock_verification_inputs(),
            (),
        );
        assert!(matches!(result, Err(Error::NetworkIsTooSmall(1))));
    }

    #[test]
    fn try_new_with_local_node_not_core() {
        let local_id = NodeId(1);
        let core_nodes = [NodeId(2)];
        let result = CoreCryptographicProcessor::<
            _,
            _,
            MockCoreAndLeaderProofsGenerator,
            MockProofsVerifier,
        >::try_new_with_core_condition_check(
            membership(&core_nodes, local_id),
            NonZeroU64::new(1).unwrap(),
            &settings(local_id),
            mock_verification_inputs(),
            (),
        );
        assert!(matches!(result, Err(Error::LocalIsNotCoreNode)));
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
            &settings(local_id),
            mock_verification_inputs(),
            (),
        );
        assert!(matches!(
            processor.decapsulate_message_recursive(mock_message),
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
            &settings(local_id),
            mock_verification_inputs(),
            (),
        );
        StaticFetchVerifier::set_remaining_valid_poq_proofs(1);
        let decapsulation_output = processor
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
            &settings(local_id),
            mock_verification_inputs(),
            (),
        );
        StaticFetchVerifier::set_remaining_valid_poq_proofs(2);
        let decapsulation_output = processor
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
            &settings(local_id),
            mock_verification_inputs(),
            (),
        );
        StaticFetchVerifier::set_remaining_valid_poq_proofs(3);
        let decapsulation_output = processor
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
            EncapsulationInput::new(
                UnsecuredEd25519Key::generate_with_blake_rng(),
                recipient_signing_pubkey,
                VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
                VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            )
        })
        .take(3)
        .collect::<Vec<_>>();
        EncapsulatedMessageWithVerifiedPublicHeader::new(
            &inputs,
            PayloadType::Cover,
            b"".as_slice().try_into().unwrap(),
        )
    }

    fn settings(local_id: NodeId) -> SessionCryptographicProcessorSettings {
        SessionCryptographicProcessorSettings {
            non_ephemeral_signing_key: key(local_id).0,
            num_blend_layers: NonZeroU64::new(1).unwrap(),
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
