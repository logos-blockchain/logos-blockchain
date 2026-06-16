#![cfg_attr(
    feature = "testing-disable-proposal-publish",
    allow(
        dead_code,
        reason = "with proposal publishing disabled for testing, some functions and struct fields are unused"
    )
)]

use std::marker::PhantomData;

use lb_blend_service::message::{NetworkMessage, ProxyServiceMessage, ServiceMessage};
use lb_chain_service_common::NetworkMessage as ChainNetworkMessage;
use lb_core::{block::Proposal, codec::SerializeOp as _};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use tracing::error;

use crate::LOG_TARGET;

pub struct BlendAdapter<BlendService>
where
    BlendService: ServiceData + lb_blend_service::ServiceComponents,
{
    relay: OutboundRelay<<BlendService as ServiceData>::Message>,
    broadcast_settings: BlendService::BroadcastSettings,
    _phantom: PhantomData<BlendService>,
}

impl<BlendService> BlendAdapter<BlendService>
where
    BlendService: ServiceData + lb_blend_service::ServiceComponents,
{
    pub const fn new(
        relay: OutboundRelay<<BlendService as ServiceData>::Message>,
        broadcast_settings: BlendService::BroadcastSettings,
    ) -> Self {
        Self {
            relay,
            broadcast_settings,
            _phantom: PhantomData,
        }
    }
}

impl<BlendService> BlendAdapter<BlendService>
where
    BlendService: ServiceData<
            Message = ProxyServiceMessage<
                ServiceMessage<BlendService::BroadcastSettings, BlendService::NodeId>,
            >,
        > + lb_blend_service::ServiceComponents
        + Sync,
    <BlendService as ServiceData>::Message: Send,
    BlendService::BroadcastSettings: Clone + Sync,
{
    pub async fn publish_proposal(&self, proposal: Proposal) {
        if let Err((e, _)) = self
            .relay
            .send(
                ServiceMessage::Blend(NetworkMessage {
                    message: ChainNetworkMessage::to_bytes(&ChainNetworkMessage::Proposal(
                        proposal,
                    ))
                    .expect("NetworkMessage should be able to be serialized")
                    .to_vec(),
                    broadcast_settings: self.broadcast_settings.clone(),
                })
                .into(),
            )
            .await
        {
            error!(target: LOG_TARGET, "Failed to relay proposal to blend service: {e:?}");
        }
    }
}
