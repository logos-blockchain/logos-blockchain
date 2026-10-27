use core::fmt::{self, Debug, Formatter};
use std::time::{SystemTime, UNIX_EPOCH};

use lb_binary_codec::{bincode::SerializeOp, canonical::BinaryEncode};
use lb_blend::message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader;
use lb_core::{
    crypto::{Digest as _, Hasher},
    mantle::NoteId,
    sdp::Locator,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_sdp_service::SdpSubmission;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::api::ApiError;

/// Information about the current Blend network peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInfo<NodeId> {
    pub node_id: NodeId,
    pub core_info: Option<CoreInfo<NodeId>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreInfo<NodeId> {
    /// Negotiated peers for the current epoch, with a flag indicating whether
    /// they are healthy (`true`) or not (`false`).
    pub current_epoch_peers: Vec<(NodeId, bool)>,
    /// Negotiated peers for the old epoch, if an epoch transition is in
    /// progress.
    pub old_epoch_peers: Option<Vec<NodeId>>,
}

pub enum ProxyServiceMessage<InnerMessage> {
    Inner(InnerMessage),
    Blend {
        payload: DataPayload,
        reply: oneshot::Sender<Result<(), ApiError>>,
    },
    JoinAsCore {
        locator: Locator,
        service_note_id: NoteId,
        reply: oneshot::Sender<Result<SdpSubmission, lb_sdp_service::api::Error>>,
    },
    GetZkId {
        reply: oneshot::Sender<ZkPublicKey>,
    },
}

impl<InnerMessage> From<InnerMessage> for ProxyServiceMessage<InnerMessage> {
    fn from(value: InnerMessage) -> Self {
        Self::Inner(value)
    }
}

/// A message that is handled by [`BlendService`].
pub enum ServiceMessage<NodeId> {
    /// To send a payload through the blend network, for the exit node to
    /// hand over to whichever local service owns that kind of payload.
    Blend(DataPayload),
    /// Request the current blend network info (connected peers).
    GetNetworkInfo {
        reply: oneshot::Sender<Option<NetworkInfo<NodeId>>>,
    },
    /// Request the transactions still waiting for a `PoW` solution to back
    /// their layer proofs, oldest first.
    // TODO: Change this to be tx IDs once we have strong types at the API level and we don't blend
    // `Vec<u8>`s but actual `SignedOps`s.
    GetPendingTransactions {
        reply: oneshot::Sender<Vec<Vec<u8>>>,
    },
}

impl<NodeId> Debug for ServiceMessage<NodeId> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blend(msg) => f.debug_tuple("Blend").field(msg).finish(),
            Self::GetNetworkInfo { .. } => f.debug_struct("GetNetworkInfo").finish(),
            Self::GetPendingTransactions { .. } => {
                f.debug_struct("GetPendingTransactions").finish()
            }
        }
    }
}

impl<NodeId> From<DataPayload> for ServiceMessage<NodeId> {
    fn from(value: DataPayload) -> Self {
        Self::Blend(value)
    }
}

/// The plaintext body of a Blend data message, tagged with what it carries.
// TODO: Replace with strong types for each message type Blend supports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum DataPayload {
    BlockProposal(Vec<u8>),
    Transaction(Vec<u8>),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum DataPayloadType {
    BlockProposal,
    Transaction,
}

impl AsRef<str> for DataPayloadType {
    fn as_ref(&self) -> &str {
        match self {
            Self::BlockProposal => "block_proposal",
            Self::Transaction => "transaction",
        }
    }
}

impl DataPayload {
    /// Encodes a transaction for blending.
    ///
    /// Whether it fits is up to the payload body size of the epoch it is sent
    /// in, which Blend checks when the payload is submitted.
    // TODO: This will go once we move away from `Vec<u8>` and into strong types
    // for each message type Blend supports. Then we can also implement `TryFrom`
    // directly.
    pub fn try_from_transaction<Tx>(transaction: &Tx) -> Result<Self, TransactionNotBlendable>
    where
        Tx: SerializeOp,
    {
        Ok(Self::Transaction(transaction.to_bytes()?.to_vec()))
    }

    /// Encodes a proposal for blending.
    ///
    /// The payload body of an era holds the largest proposal of the era, which
    /// Blend checks when the payload is submitted.
    // TODO: This will go once we move away from `Vec<u8>` and into strong types
    // for each message type Blend supports. Then we can also implement `From`
    // directly.
    pub fn from_proposal<Proposal>(proposal: &Proposal) -> Self
    where
        Proposal: BinaryEncode,
    {
        Self::BlockProposal(proposal.encode_to_vec())
    }

    /// The payload, if it fits a payload body of `payload_body_size` bytes.
    ///
    /// # Errors
    ///
    /// [`PayloadTooLarge`] if it does not.
    pub(super) fn validate_size(
        self,
        payload_body_size: u16,
    ) -> Result<DataPayloadWithValidatedSize, PayloadTooLarge> {
        if self.len() > usize::from(payload_body_size) {
            return Err(PayloadTooLarge {
                size: self.len(),
                maximum: payload_body_size,
            });
        }
        Ok(DataPayloadWithValidatedSize(self))
    }

    /// The wire discriminant this payload travels under.
    #[must_use]
    pub const fn payload_type(&self) -> DataPayloadType {
        match self {
            Self::BlockProposal(_) => DataPayloadType::BlockProposal,
            Self::Transaction(_) => DataPayloadType::Transaction,
        }
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        match self {
            Self::BlockProposal(body) | Self::Transaction(body) => body,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.body().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.body().is_empty()
    }
}

/// A payload that fits a payload body of the epoch it was submitted in, built
/// only by [`DataPayload::validate_size`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPayloadWithValidatedSize(DataPayload);

impl DataPayloadWithValidatedSize {
    #[must_use]
    pub fn into_inner(self) -> DataPayload {
        self.0
    }
}

/// Produces a stable digest for the exact canonical proposal bytes carried by
/// a [`DataPayload::BlockProposal`].
#[must_use]
pub fn proposal_diagnostic_digest_from_encoded(encoded_proposal: &[u8]) -> String {
    hex::encode(Hasher::digest(encoded_proposal))
}

/// Returns a wall-clock timestamp for correlating proposal diagnostic events.
#[must_use]
pub fn proposal_diagnostic_timestamp_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::UpperBoundedVec;

    use super::*;

    #[test]
    fn proposal_diagnostic_digest_correlates_without_changing_payload_bytes() {
        let proposal = UpperBoundedVec::<u8, 4>::new_unchecked(vec![1, 2, 3, 4]);
        let payload = DataPayload::from_proposal(&proposal);
        let encoded = payload.body().to_vec();

        let submitted_digest = proposal_diagnostic_digest_from_encoded(&encoded);
        let exit_digest = proposal_diagnostic_digest_from_encoded(payload.body());

        assert_eq!(submitted_digest, exit_digest);
        assert_eq!(submitted_digest.len(), 64);
        assert!(
            submitted_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
        assert_eq!(payload.body(), encoded);
    }

    #[test]
    fn different_proposal_bytes_have_different_diagnostic_digests() {
        let first = [1, 2, 3];
        let second = [1, 2, 4];

        assert_ne!(
            proposal_diagnostic_digest_from_encoded(&first),
            proposal_diagnostic_digest_from_encoded(&second)
        );
    }
}

/// Why Blend refuses a payload: it is larger than a payload body of the
/// current epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "Payload of {size} bytes exceeds the {maximum} a Blend payload carries in the current epoch."
)]
pub struct PayloadTooLarge {
    pub size: usize,
    pub maximum: u16,
}

/// Why a transaction cannot be carried by the Blend network.
#[derive(Debug, thiserror::Error)]
pub enum TransactionNotBlendable {
    #[error("Transaction cannot be encoded: {0}")]
    Encoding(#[from] lb_binary_codec::bincode::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ProcessedMessage {
    Decapsulated(DataPayload),
    Encapsulated(Box<EncapsulatedMessageWithVerifiedPublicHeader>),
}

impl From<DataPayload> for ProcessedMessage {
    fn from(value: DataPayload) -> Self {
        Self::Decapsulated(value)
    }
}

impl From<EncapsulatedMessageWithVerifiedPublicHeader> for ProcessedMessage {
    fn from(value: EncapsulatedMessageWithVerifiedPublicHeader) -> Self {
        Self::Encapsulated(Box::new(value))
    }
}
