use core::future::ready;

use futures::{Stream, StreamExt as _, stream, stream::BoxStream};
use lb_chain_service_common::NetworkMessage as ChainNetworkMessage;
use lb_core::codec::DeserializeOp as _;
use lb_network_service::{
    NetworkService,
    backends::libp2p::{Command, Libp2p, Message, PubSubCommand},
    message::NetworkMsg,
};
use lb_tracing::info_with_id;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use super::NetworkAdapter;
use crate::message::NetworkMessage;

/// A network adapter for the network service that uses libp2p backend.
#[derive(Clone)]
pub struct Libp2pAdapter<RuntimeServiceId> {
    network_relay:
        OutboundRelay<<NetworkService<Libp2p, RuntimeServiceId> as ServiceData>::Message>,
}

/// Settings used to broadcast messages to the network service that uses libp2p
/// backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Libp2pBroadcastSettings {
    pub topic: String,
}

fn stop_observing_on_lag<Observed>(
    subscription: BroadcastStream<Observed>,
) -> impl Stream<Item = Observed> + Send
where
    Observed: Clone + Send + 'static,
{
    subscription
        .take_while(|subscribed| {
            ready(match subscribed {
                Ok(_) => true,
                Err(BroadcastStreamRecvError::Lagged(missed)) => {
                    tracing::error!("Missed {missed} broadcasting-channel messages; a delivery can no longer be told from a loss, so the direct broadcast is disabled for the rest of this run.");
                    false
                }
            })
        })
        .filter_map(|subscribed| ready(subscribed.ok()))
}

#[async_trait::async_trait]
impl<RuntimeServiceId> NetworkAdapter<RuntimeServiceId> for Libp2pAdapter<RuntimeServiceId> {
    type Backend = Libp2p;
    type BroadcastSettings = Libp2pBroadcastSettings;

    fn new(
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        Self { network_relay }
    }

    /// Broadcast an unencrypted message to the network by publishing the
    /// message under the configured gossipsub topic.
    async fn broadcast(&self, message: Vec<u8>, broadcast_settings: Self::BroadcastSettings) {
        if let Ok(ChainNetworkMessage::Proposal(proposal)) =
            ChainNetworkMessage::from_bytes(&message)
        {
            info_with_id!(proposal.header.id().as_ref(), "broadcasting proposal");
        }

        if let Err((e, _)) = self
            .network_relay
            .send(NetworkMsg::Process(Command::PubSub(
                PubSubCommand::Broadcast {
                    topic: broadcast_settings.topic.clone(),
                    message: message.into_boxed_slice(),
                },
            )))
            .await
        {
            tracing::error!("error broadcasting message: {e}");
        }
    }

    async fn observe_broadcasts(
        &self,
    ) -> BoxStream<'static, NetworkMessage<Self::BroadcastSettings>> {
        let network_relay = self.network_relay.clone();
        let (sender, receiver) = oneshot::channel();
        if let Err((e, _)) = network_relay
            .send(NetworkMsg::SubscribeToPubSub { sender })
            .await
        {
            tracing::error!("Failed to ask the network service for the broadcasting channel: {e}");
            return stream::empty().boxed();
        }
        let Ok(broadcasts) = receiver.await else {
            tracing::error!("The network service dropped the broadcasting-channel subscription.");
            return stream::empty().boxed();
        };

        stop_observing_on_lag(broadcasts)
            .map(|Message { data, topic, .. }| NetworkMessage {
                message: data,
                broadcast_settings: Libp2pBroadcastSettings {
                    topic: topic.into_string(),
                },
            })
            .boxed()
    }
}
