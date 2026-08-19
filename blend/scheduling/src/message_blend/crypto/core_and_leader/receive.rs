use lb_blend_message::{
    Error,
    crypto::proofs::PoQVerificationInputsMinusSigningKey,
    encap::{
        ProofsVerifier as ProofsVerifierTrait,
        decapsulated::{DecapsulatedMessage, DecapsulationOutput},
        encapsulated::EncapsulatedMessage,
        validated::RequiredProofOfSelectionVerificationInputs,
    },
    reward::BlendingToken,
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::X25519PrivateKey;

use crate::{
    membership::Membership, message_blend::crypto::EncapsulatedMessageWithVerifiedPublicHeader,
};

/// [`EpochCryptographicProcessor`] is responsible for only unwrapping the
/// messages addressed to the local node.
///
/// Each instance is meant to be used during a single epoch.
///
/// It holds no proof generator, and hence cannot encapsulate anything:
/// receiving spends no quota, so there is nothing for it to prove. That is what
/// makes it the type for an epoch that has ended, which is kept around for the
/// transition period only so that messages still in flight from it can be
/// decapsulated and forwarded. Converting that epoch's send-and-receive
/// processor with
/// [`into_receive_only`](super::send_and_receive::EpochCryptographicProcessor::into_receive_only)
/// drops its generators, and with them the `PoW` mining stream that would
/// otherwise keep a core searching for solutions to a puzzle nobody will accept
/// an answer to anymore.
pub struct EpochCryptographicProcessor<ProofsVerifier> {
    /// The non-ephemeral encryption key (NEK) for decapsulating messages.
    non_ephemeral_encryption_key: X25519PrivateKey,
    /// Index of the local node in the epoch's membership, `None` if the local
    /// node is not a core node in it. The membership of an epoch does not
    /// change, so this and the size below are resolved once, on construction.
    local_node_index: Option<usize>,
    membership_size: usize,
    proofs_verifier: ProofsVerifier,
    epoch: Epoch,
}

impl<ProofsVerifier> EpochCryptographicProcessor<ProofsVerifier> {
    pub const fn verifier(&self) -> &ProofsVerifier {
        &self.proofs_verifier
    }

    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }
}

impl<ProofsVerifier> EpochCryptographicProcessor<ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait,
{
    #[must_use]
    pub fn new<NodeId>(
        non_ephemeral_encryption_key: X25519PrivateKey,
        membership: &Membership<NodeId>,
        public_info: PoQVerificationInputsMinusSigningKey,
        epoch: Epoch,
    ) -> Self {
        Self {
            non_ephemeral_encryption_key,
            local_node_index: membership.local_index(),
            membership_size: membership.size(),
            proofs_verifier: ProofsVerifier::new(public_info),
            epoch,
        }
    }

    pub fn decapsulate_message(
        &self,
        message: EncapsulatedMessageWithVerifiedPublicHeader,
    ) -> Result<DecapsulationOutput, Error> {
        let Some(local_node_index) = self.local_node_index else {
            return Err(Error::NotCoreNodeReceiver);
        };
        message.decapsulate(
            &self.non_ephemeral_encryption_key,
            &RequiredProofOfSelectionVerificationInputs {
                expected_node_index: local_node_index as u64,
                total_membership_size: self.membership_size as u64,
            },
            &self.proofs_verifier,
        )
    }

    /// Validate the public header of an [`EncapsulatedMessage`].
    pub fn validate_message_header(
        &self,
        message: EncapsulatedMessage,
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
        message.verify_public_header(&self.proofs_verifier)
    }

    /// Semantically similar to [`Self::decapsulate_message`], but it does not
    /// stop after decapsulating the outermost layer. It stops only when a layer
    /// cannot be decapsulated or when the decapsulation is completed.
    ///
    /// If no layer (`Err`) or at most one layer (`Ok`) can be decapsulated,
    /// this is semantically equivalent to calling
    /// [`Self::decapsulate_message`].
    ///
    /// If more than a single layer can be decapsulated, then the decapsulation
    /// happens recursively until the first layer that cannot be decapsulated is
    /// found or when there is no more layers to decapsulate. In either case, it
    /// returns the last processed layer, along with the list of blending tokens
    /// collected along the way.
    pub fn decapsulate_message_recursive(
        &self,
        message: EncapsulatedMessageWithVerifiedPublicHeader,
    ) -> Result<MultiLayerDecapsulationOutput, Error> {
        tracing::trace!(
            "Attempt at batch-decapsulating message with PoQ nullifier and key: ({:?}, {:?})",
            message.public_header().signing_key(),
            message.public_header().proof_of_quota().key_nullifier()
        );
        let mut decapsulation_output = self.decapsulate_message(message)?;

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
                    let Ok(message_with_validated_public_header) =
                        self.validate_message_header((**remaining_encapsulated_message).clone())
                    else {
                        break;
                    };
                    let Ok(nested_layer_decapsulation_output) =
                        self.decapsulate_message(message_with_validated_public_header)
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

/// The output of a multi-layer decapsulation operation.
#[derive(Debug)]
pub struct MultiLayerDecapsulationOutput {
    /// The blending token collected on the way, one per decapsulated layer.
    blending_tokens: Vec<BlendingToken>,
    /// The final message type.
    decapsulated_message: DecapsulatedMessageType,
}

impl MultiLayerDecapsulationOutput {
    #[must_use]
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
