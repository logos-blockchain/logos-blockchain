use core::fmt::{self, Debug, Formatter};

use lb_blend::message::{
    PayloadType, encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
};
use lb_core::{
    mantle::NoteId,
    sdp::{DeclarationId, Locator},
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

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
    JoinAsCore {
        locator: Locator,
        locked_note_id: NoteId,
        reply: oneshot::Sender<Result<DeclarationId, lb_sdp_service::api::Error>>,
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
    Blend(BlendPayload),
    /// Request the current blend network info (connected peers).
    GetNetworkInfo {
        reply: oneshot::Sender<Option<NetworkInfo<NodeId>>>,
    },
}

impl<NodeId> Debug for ServiceMessage<NodeId> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blend(msg) => f.debug_tuple("Blend").field(msg).finish(),
            Self::GetNetworkInfo { .. } => f.debug_struct("GetNetworkInfo").finish(),
        }
    }
}

/// The plaintext body of a Blend data message, tagged with what it carries.
// TODO: Replace with strong types for each message type Blend supports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum BlendPayload {
    BlockProposal(Vec<u8>),
    Transaction(Vec<u8>),
}

impl BlendPayload {
    /// The wire discriminant this payload travels under.
    #[must_use]
    pub const fn payload_type(&self) -> PayloadType {
        match self {
            Self::BlockProposal(_) => PayloadType::BlockProposal,
            Self::Transaction(_) => PayloadType::Transaction,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ProcessedMessage {
    Unencapsulated(BlendPayload),
    Encapsulated(Box<EncapsulatedMessageWithVerifiedPublicHeader>),
}

impl From<BlendPayload> for ProcessedMessage {
    fn from(value: BlendPayload) -> Self {
        Self::Unencapsulated(value)
    }
}

impl From<EncapsulatedMessageWithVerifiedPublicHeader> for ProcessedMessage {
    fn from(value: EncapsulatedMessageWithVerifiedPublicHeader) -> Self {
        Self::Encapsulated(Box::new(value))
    }
}
