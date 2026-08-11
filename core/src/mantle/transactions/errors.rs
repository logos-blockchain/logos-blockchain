use crate::mantle::ops::channel::{ChannelId, ChannelKeyIndex};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum VerificationError {
    #[error("Missing required proof for {op_type} operation at index {op_index}")]
    MissingProof {
        op_type: &'static str,
        op_index: usize,
    },
    #[error("Incorrect proof type for {op_type} operation at index {op_index}")]
    IncorrectProofType {
        op_type: &'static str,
        op_index: usize,
    },
    #[error(
        "The number of proofs ({proofs_count}) does not match the number of operations ({ops_count})"
    )]
    ProofCountMismatch {
        ops_count: usize,
        proofs_count: usize,
    },
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
    #[error("Duplicate signature indices in ChannelMultiSigProof at index {op_index}")]
    ChannelMultiSigProofDuplicateIndices { op_index: usize },
    #[error(
        "Invalid signature in ChannelMultiSigProof at index {op_index} for signature index {signature_index}"
    )]
    ChannelMultiSigProofInvalidSignature {
        op_index: usize,
        signature_index: usize,
    },
    #[error("Channel verification error: {0}")]
    ChannelVerificationError(crate::mantle::channel::Error),
    #[error("Transfer verification error: {0}")]
    TransferVerificationError(#[from] crate::mantle::ops::transfer::TransferError),
    #[error("SDP verification error: {0}")]
    SDPVerificationError(crate::mantle::ops::sdp::SdpError),
    #[error("LeaderClaim verification error: {0}")]
    LeaderClaimVerificationError(crate::mantle::ops::leader_claim::LeaderClaimError),
    #[error("ClaimPoWReward verification error: {0}")]
    ClaimPowRewardError(crate::mantle::ops::pow::ClaimPowRewardError),
}
