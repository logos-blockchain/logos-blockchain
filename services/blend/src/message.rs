use core::fmt::{self, Debug, Formatter};

use lb_blend::message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader;
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
