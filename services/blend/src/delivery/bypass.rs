use futures::future::join_all;

use crate::{LOG_TARGET, core::dispatcher::PayloadDispatcher, message::BlendPayload, metrics};

/// Broadcasts, in the clear, the payloads the Blend network did not deliver
/// within the delivery deadline.
pub async fn broadcast_undelivered_payloads<Dispatcher, UndeliveredPayloads, RuntimeServiceId>(
    undelivered: UndeliveredPayloads,
    payload_dispatcher: &Dispatcher,
) where
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
    UndeliveredPayloads: ExactSizeIterator<Item = BlendPayload> + Send,
{
    // TODO: Once we switch to a well-defined API for blending payloads, we can and
    // should show the relevant details for each payload that failed. E.g., for
    // blocks we could log the block ID.
    tracing::warn!(
        target: LOG_TARGET,
        "The Blend network did not deliver {} locally-originated payload(s) within the delivery deadline; broadcasting them directly.",
        undelivered.len()
    );
    join_all(undelivered.into_iter().map(|payload| {
        metrics::payload_bypassed_blend(payload.payload_type());
        payload_dispatcher.dispatch(payload)
    }))
    .await;
}
