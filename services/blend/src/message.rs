use lb_blend::message::encap::encapsulated::EncapsulatedMessage;
use serde::{Deserialize, Serialize};

/// A message that is handled by [`BlendService`].
#[derive(Debug)]
pub enum ServiceMessage<BroadcastSettings> {
    /// To send a message to the blend network and eventually broadcast it to
    /// the [`NetworkService`].
    Blend(NetworkMessage<BroadcastSettings>),
}

/// A message that is sent to the blend network.
///
/// To eventually broadcast the message to the network service,
/// [`BroadcastSettings`] must be included in the [`NetworkMessage`].
/// [`BroadcastSettings`] is a generic type defined by [`NetworkAdapter`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct NetworkMessage<BroadcastSettings> {
    pub message: Vec<u8>,
    pub broadcast_settings: BroadcastSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ProcessedMessage<BroadcastSettings> {
    Network(NetworkMessage<BroadcastSettings>),
    // We cannot use `EncapsulatedMessageWithVerifiedPublicHeader` because we don't know if this
    // message belongs to the current or the old session, so we need to let the libp2p swarm find
    // out.
    Encapsulated(Box<EncapsulatedMessage>),
}

impl<BroadcastSettings> From<NetworkMessage<BroadcastSettings>>
    for ProcessedMessage<BroadcastSettings>
{
    fn from(value: NetworkMessage<BroadcastSettings>) -> Self {
        Self::Network(value)
    }
}

impl<BroadcastSettings> From<EncapsulatedMessage> for ProcessedMessage<BroadcastSettings> {
    fn from(value: EncapsulatedMessage) -> Self {
        Self::Encapsulated(Box::new(value))
    }
}
