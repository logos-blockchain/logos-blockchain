use lb_codec::BinaryDecode as _;
use lb_core::block::Proposal;
use lb_network_service::{
    NetworkService,
    backends::libp2p::{Command, Libp2p, PubSubCommand},
    message::NetworkMsg,
};
use lb_tracing::info_with_id;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Deserialize, Serialize};

use super::NetworkAdapter;

/// A network adapter for the network service that uses libp2p backend.
#[derive(Clone)]
pub struct Libp2pAdapter<RuntimeServiceId> {
    network_relay:
        OutboundRelay<<NetworkService<Libp2p, RuntimeServiceId> as ServiceData>::Message>,
    settings: Libp2pBroadcastSettings,
}

/// Settings used to broadcast messages to the network service that uses libp2p
/// backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Libp2pBroadcastSettings {
    pub topic: String,
}

#[async_trait::async_trait]
impl<RuntimeServiceId> NetworkAdapter<RuntimeServiceId> for Libp2pAdapter<RuntimeServiceId> {
    type Backend = Libp2p;
    type Settings = Libp2pBroadcastSettings;

    fn new(
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
        settings: Self::Settings,
    ) -> Self {
        Self {
            network_relay,
            settings,
        }
    }

    /// Broadcast an unencrypted message to the network by publishing the
    /// message under the configured gossipsub topic.
    async fn broadcast(&self, message: Vec<u8>) {
        if let Ok(proposal) = Proposal::decode_all(&message, &()) {
            info_with_id!(proposal.header.id().as_ref(), "broadcasting proposal");
        }

        if let Err((e, _)) = self
            .network_relay
            .send(NetworkMsg::Process(Command::PubSub(
                PubSubCommand::Broadcast {
                    topic: self.settings.topic.clone(),
                    message: message.into_boxed_slice(),
                },
            )))
            .await
        {
            tracing::error!("error broadcasting message: {e}");
        }
    }
}
