use lb_utils::bounded::BoundedError;

use crate::mantle::{
    channel::Error as ChannelError,
    ops::{
        channel::{ChannelId, ChannelKeyIndex},
        sdp::SdpError,
    },
};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum VerificationError {
    #[error("Channel {channel_id} could not be found")]
    ChannelNotFound { channel_id: ChannelId },
    #[error("Key {key_index} could not be found in channel {channel_id}")]
    KeyNotFound {
        channel_id: ChannelId,
        key_index: ChannelKeyIndex,
    },
    #[error(
        "Not enough signatures in ChannelMultiSigProof at index {op_index}: got {actual}, required {required}"
    )]
    ChannelMultiSigProofNotEnoughSignatures {
        op_index: usize,
        actual: usize,
        required: ChannelKeyIndex,
    },
    #[error(
        "Invalid signature in ChannelMultiSigProof at index {op_index} for signature index {signature_index}"
    )]
    ChannelMultiSigProofInvalidSignature {
        op_index: usize,
        signature_index: usize,
    },
    #[error("Channel verification error: {0}")]
    ChannelVerificationError(#[from] crate::mantle::channel::Error),
    #[error("Transfer verification error: {0}")]
    TransferVerificationError(#[from] crate::mantle::ops::transfer::TransferError),
    #[error("ProgramRegister verification error: {0}")]
    ProgramRegisterError(#[from] crate::mantle::ops::program::ProgramRegisterError),
    #[error("SDP verification error: {0}")]
    SDPVerificationError(#[from] SdpError),
    #[error("ClaimPoWReward verification error: {0}")]
    ClaimPowRewardError(#[from] crate::mantle::ops::pow::ClaimPowRewardError),
    #[error(transparent)]
    BoundsError(#[from] BoundedError),
}

impl VerificationError {
    /// Whether an Operation proof failed, rather than the Operations reading
    /// the state. Block Header Validation: the block ID commits to the
    /// Operations and not to their proofs, so such a failure condemns one copy
    /// of a block and not its ID.
    #[must_use]
    pub const fn is_operation_proof_failure(&self) -> bool {
        matches!(
            self,
            Self::ChannelMultiSigProofNotEnoughSignatures { .. }
                | Self::ChannelMultiSigProofInvalidSignature { .. }
                | Self::ChannelVerificationError(
                    ChannelError::InvalidSignature
                        | ChannelError::InvalidSignatureIndex { .. }
                        | ChannelError::ThresholdUnmet { .. }
                        | ChannelError::StepProofCount { .. }
                        | ChannelError::StepProofKind { .. }
                        | ChannelError::PaymentProofCount { .. }
                        | ChannelError::ProgramNoteCount { .. }
                        | ChannelError::ProgramNoteOpening { .. }
                )
                | Self::SDPVerificationError(SdpError::InvalidEddsaSignature)
        )
    }
}
