use core::fmt::{self, Debug, Formatter};

use lb_blend::message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader;
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
pub enum ServiceMessage<BroadcastSettings, NodeId> {
    /// To send a message to the blend network and eventually broadcast it to
    /// the [`NetworkService`].
    Blend(NetworkMessage<BroadcastSettings>),
    /// Request the current blend network info (connected peers).
    GetNetworkInfo {
        reply: oneshot::Sender<Option<NetworkInfo<NodeId>>>,
    },
}

impl<BroadcastSettings, NodeId> Debug for ServiceMessage<BroadcastSettings, NodeId>
where
    BroadcastSettings: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blend(msg) => f.debug_tuple("Blend").field(msg).finish(),
            Self::GetNetworkInfo { .. } => f.debug_struct("GetNetworkInfo").finish(),
        }
    }
}

/// A message that is sent to the blend network.
///
/// To eventually broadcast the message to the network service,
/// [`BroadcastSettings`] must be included in the [`NetworkMessage`].
/// [`BroadcastSettings`] is a generic type defined by [`NetworkAdapter`].
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct NetworkMessage<BroadcastSettings> {
    pub message: Vec<u8>,
    pub broadcast_settings: BroadcastSettings,
}

impl<BroadcastSettings> Debug for NetworkMessage<BroadcastSettings>
where
    BroadcastSettings: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkMessage")
            .field("message", &format_args!("{} bytes", self.message.len()))
            .field("broadcast_settings", &self.broadcast_settings)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ProcessedMessage<BroadcastSettings> {
    Network(NetworkMessage<BroadcastSettings>),
    Encapsulated(Box<EncapsulatedMessageWithVerifiedPublicHeader>),
}

impl<BroadcastSettings> From<NetworkMessage<BroadcastSettings>>
    for ProcessedMessage<BroadcastSettings>
{
    fn from(value: NetworkMessage<BroadcastSettings>) -> Self {
        Self::Network(value)
    }
}

impl<BroadcastSettings> From<EncapsulatedMessageWithVerifiedPublicHeader>
    for ProcessedMessage<BroadcastSettings>
{
    fn from(value: EncapsulatedMessageWithVerifiedPublicHeader) -> Self {
        Self::Encapsulated(Box::new(value))
    }
}
