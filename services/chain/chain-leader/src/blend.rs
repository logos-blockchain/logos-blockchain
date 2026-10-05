#![cfg_attr(
    feature = "testing-disable-proposal-publish",
    allow(
        dead_code,
        reason = "with proposal publishing disabled for testing, some functions and struct fields are unused"
    )
)]

use std::marker::PhantomData;

use lb_blend_service::message::{DataPayload, ProxyServiceMessage, ServiceMessage};
use lb_core::block::Proposal;
use lb_log_targets::chain;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use tracing::error;

const LOG_TARGET: &str = chain::leader::BLEND;

pub struct BlendAdapter<BlendService>
where
    BlendService: ServiceData + lb_blend_service::ServiceComponents,
{
    relay: OutboundRelay<<BlendService as ServiceData>::Message>,
    // `fn() -> BlendService` (rather than `PhantomData<BlendService>`) so the
    // adapter's `Send`/`Sync` do not depend on `BlendService`'s — the adapter
    // only uses `BlendService` as a type-level tag for the relay message type,
    // and is held by shared reference across awaits in the leader run loop.
    _phantom: PhantomData<fn() -> BlendService>,
}

impl<BlendService> BlendAdapter<BlendService>
where
    BlendService: ServiceData + lb_blend_service::ServiceComponents,
{
    pub const fn new(relay: OutboundRelay<<BlendService as ServiceData>::Message>) -> Self {
        Self {
            relay,
            _phantom: PhantomData,
        }
    }
}

impl<BlendService> BlendAdapter<BlendService>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<BlendService::NodeId>>>
        + lb_blend_service::ServiceComponents,
    <BlendService as ServiceData>::Message: Send,
{
    pub async fn publish_proposal(&self, proposal: Proposal) {
        let Ok(payload) = DataPayload::try_from_proposal(&proposal) else {
            error!(
                target: LOG_TARGET,
                "Refusing to publish an oversized block proposal"
            );
            return;
        };

        if let Err(error) = self.relay.send(ServiceMessage::Blend(payload).into()).await {
            error!(target: LOG_TARGET, "Failed to relay proposal to blend service: {error}");
        }
    }
}
