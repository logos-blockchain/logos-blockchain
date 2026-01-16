use futures::channel::oneshot::Canceled;
use libp2p::PeerId;
use libp2p_stream::OpenStreamError;
use nomos_core::{codec, da::BlobId, sdp::SessionNumber};
use nomos_da_messages::sampling;
use thiserror::Error;

use super::BehaviourSampleReq;
use crate::{SubnetworkId, protocols::sampling::opinions::OpinionEvent};

#[derive(Debug, Error)]
pub enum SamplingError {
    #[error("Stream disconnected: {error}")]
    Io {
        peer_id: PeerId,
        error: std::io::Error,
        message: Option<sampling::SampleRequest>,
    },
    #[error("Share response error: {error:?}")]
    Share {
        subnetwork_id: SubnetworkId,
        peer_id: PeerId,
        error: sampling::ShareError,
    },
    #[error("Commitments response error: {error:?}")]
    Commitments {
        peer_id: PeerId,
        error: sampling::CommitmentsError,
    },
    #[error("Error opening stream [{peer_id}]: {error}")]
    OpenStream {
        peer_id: PeerId,
        error: OpenStreamError,
    },
    #[error("Unable to deserialize blob response: {error}")]
    Deserialize {
        blob_id: BlobId,
        subnetwork_id: SubnetworkId,
        peer_id: PeerId,
        error: codec::Error,
    },
    #[error("Error sending request: {request:?}")]
    RequestChannel {
        request: BehaviourSampleReq,
        peer_id: PeerId,
    },
    #[error("Malformed blob id: {blob_id:?}")]
    InvalidBlobId { peer_id: PeerId, blob_id: Vec<u8> },
    #[error("Blob not found: {blob_id:?}")]
    BlobNotFound {
        peer_id: PeerId,
        blob_id: Vec<u8>,
        subnetwork_id: SubnetworkId,
    },
    #[error("Canceled response: {error}")]
    ResponseChannel { error: Canceled, peer_id: PeerId },
    #[error("Peer {peer_id} sent share for subnet {received} instead of {expected}")]
    MismatchSubnetwork {
        expected: SubnetworkId,
        received: SubnetworkId,
        peer_id: PeerId,
    },
    #[error("Failed to dial peers in {subnetwork_id} for blob {blob_id:?}")]
    NoSubnetworkPeers {
        blob_id: BlobId,
        subnetwork_id: SubnetworkId,
    },
    #[error("Mismatch session {session} for blob {blob_id:?}")]
    MismatchSession {
        session: SessionNumber,
        blob_id: BlobId,
    },

    #[error("Mismatch session {session} for blob {blob_id:?}")]
    CommitmentsMismatchSession {
        session: SessionNumber,
        blob_id: BlobId,
    },
}

impl SamplingError {
    #[must_use]
    pub const fn peer_id(&self) -> Option<&PeerId> {
        match self {
            Self::Io { peer_id, .. }
            | Self::Share { peer_id, .. }
            | Self::Commitments { peer_id, .. }
            | Self::OpenStream { peer_id, .. }
            | Self::Deserialize { peer_id, .. }
            | Self::RequestChannel { peer_id, .. }
            | Self::ResponseChannel { peer_id, .. }
            | Self::InvalidBlobId { peer_id, .. }
            | Self::MismatchSubnetwork { peer_id, .. }
            | Self::BlobNotFound { peer_id, .. } => Some(peer_id),
            Self::MismatchSession { .. }
            | Self::NoSubnetworkPeers { .. }
            | Self::CommitmentsMismatchSession { .. } => None,
        }
    }

    #[must_use]
    pub fn blob_id(&self) -> Option<&BlobId> {
        match self {
            Self::BlobNotFound { blob_id, .. } => blob_id.as_slice().try_into().ok(),
            Self::Deserialize { blob_id, .. } => Some(blob_id),
            Self::Share { error, .. } => Some(&error.blob_id),
            _ => None,
        }
    }
}

impl Clone for SamplingError {
    fn clone(&self) -> Self {
        match self {
            Self::Io {
                peer_id,
                error,
                message,
            } => Self::Io {
                peer_id: *peer_id,
                error: std::io::Error::new(error.kind(), error.to_string()),
                message: *message,
            },
            Self::Share {
                subnetwork_id,
                peer_id,
                error,
            } => Self::Share {
                subnetwork_id: *subnetwork_id,
                peer_id: *peer_id,
                error: error.clone(),
            },
            Self::Commitments { peer_id, error } => Self::Commitments {
                peer_id: *peer_id,
                error: error.clone(),
            },
            Self::OpenStream { peer_id, error } => Self::OpenStream {
                peer_id: *peer_id,
                error: match error {
                    OpenStreamError::UnsupportedProtocol(protocol) => {
                        OpenStreamError::UnsupportedProtocol(protocol.clone())
                    }
                    OpenStreamError::Io(error) => {
                        OpenStreamError::Io(std::io::Error::new(error.kind(), error.to_string()))
                    }
                    err => OpenStreamError::Io(std::io::Error::other(err.to_string())),
                },
            },
            Self::Deserialize {
                blob_id,
                subnetwork_id,
                peer_id,
                error,
            } => Self::Deserialize {
                blob_id: *blob_id,
                subnetwork_id: *subnetwork_id,
                peer_id: *peer_id,
                error: error.clone(),
            },
            Self::RequestChannel { request, peer_id } => Self::RequestChannel {
                request: request.clone(),
                peer_id: *peer_id,
            },
            Self::ResponseChannel { error, peer_id } => Self::ResponseChannel {
                peer_id: *peer_id,
                error: *error,
            },
            Self::InvalidBlobId { blob_id, peer_id } => Self::InvalidBlobId {
                peer_id: *peer_id,
                blob_id: blob_id.clone(),
            },
            Self::BlobNotFound {
                blob_id,
                peer_id,
                subnetwork_id,
            } => Self::BlobNotFound {
                peer_id: *peer_id,
                blob_id: blob_id.clone(),
                subnetwork_id: *subnetwork_id,
            },
            Self::MismatchSubnetwork {
                expected,
                received,
                peer_id,
            } => Self::MismatchSubnetwork {
                expected: *expected,
                received: *received,
                peer_id: *peer_id,
            },
            Self::NoSubnetworkPeers {
                blob_id,
                subnetwork_id,
            } => Self::NoSubnetworkPeers {
                blob_id: *blob_id,
                subnetwork_id: *subnetwork_id,
            },
            Self::MismatchSession { session, blob_id } => Self::MismatchSession {
                session: *session,
                blob_id: *blob_id,
            },
            Self::CommitmentsMismatchSession { session, blob_id } => {
                Self::CommitmentsMismatchSession {
                    session: *session,
                    blob_id: *blob_id,
                }
            }
        }
    }
}

#[derive(Error, Debug, Clone)]
pub enum HistoricSamplingError {
    #[error("Historic sampling failed")]
    SamplingFailed(OpinionEvent),
    #[error("Internal server error: {0}")]
    InternalServerError(String),
}

#[derive(Error, Debug, Clone)]
pub enum HistoricCommitmentsError {
    #[error("Historic commitments sampling failed")]
    CommitmentsFailed(OpinionEvent),
    #[error("Internal server error: {0}")]
    InternalServerError(String),
}

impl From<HistoricSamplingError> for HistoricCommitmentsError {
    fn from(err: HistoricSamplingError) -> Self {
        match err {
            HistoricSamplingError::SamplingFailed(opinion) => Self::CommitmentsFailed(opinion),
            HistoricSamplingError::InternalServerError(msg) => Self::InternalServerError(msg),
        }
    }
}
