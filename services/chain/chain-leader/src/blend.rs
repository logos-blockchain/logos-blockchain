#![cfg_attr(
    feature = "testing-disable-proposal-publish",
    allow(
        dead_code,
        reason = "with proposal publishing disabled for testing, some functions and struct fields are unused"
    )
)]

use std::marker::PhantomData;

use lb_blend_service::{
    api::{BlendServiceApi, BlendServiceData},
    message::{
        DataPayload, proposal_diagnostic_digest_from_encoded, proposal_diagnostic_timestamp_unix_ms,
    },
};
use lb_core::block::Proposal;
use lb_log_targets::chain;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use tracing::{debug, error};

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
    BlendService: BlendServiceData<NodeId: Send>,
{
    pub async fn publish_proposal(&self, proposal: Proposal) {
        let payload = DataPayload::from_proposal(&proposal);
        let payload_clone = payload.clone();

        // Blend refuses a proposal larger than a payload body of its current
        // epoch.
        if let Err(error) = BlendServiceApi::<BlendService, ()>::new(self.relay.clone())
            .publish(payload)
            .await
        {
            error!(target: LOG_TARGET, "Failed to publish our block proposal through Blend: {error}");
            return;
        }

        log_proposal_submitted(&proposal, &payload_clone);
    }
}

fn log_proposal_submitted(proposal: &Proposal, payload: &DataPayload) {
    debug!(
        target: LOG_TARGET,
        event = "blend_proposal_submitted",
        timestamp_unix_ms = proposal_diagnostic_timestamp_unix_ms(),
        block_id = %proposal.header().id(),
        slot = u64::from(proposal.header().slot()),
        proposal_digest = %proposal_diagnostic_digest_from_encoded(payload.body()),
        "Successfully enqueued local block proposal to Blend"
    );
}
