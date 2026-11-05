use futures::{Stream, StreamExt as _, future::join_all};
use lb_cryptarchia_engine::Era;

pub use self::failure_detection::FailureDetector;
use crate::{LOG_TARGET, core::dispatcher::PayloadDispatcher, message::DataPayload, metrics};

mod failure_detection;

#[cfg(test)]
pub mod test_utils;

/// The payloads whose deadline has just passed, from a sender that is watching
/// for them.
///
/// A sender that is not — the operator turned the fallback off — waits forever
/// instead, which leaves the `select!` branch this feeds permanently idle
/// rather than firing.
pub async fn next_undelivered_messages<Detection>(
    failure_detection: Option<&mut Detection>,
) -> Option<Vec<DataPayload>>
where
    Detection: Stream<Item = Vec<DataPayload>> + Unpin + Send,
{
    match failure_detection {
        Some(failure_detection) => failure_detection.next().await,
        None => core::future::pending().await,
    }
}

/// Broadcasts, in the clear and over the identifiers of `era`, the payloads the
/// Blend network did not deliver within the delivery deadline.
pub async fn broadcast_undelivered_messages<Dispatcher, UndeliveredPayloads, RuntimeServiceId>(
    undelivered: UndeliveredPayloads,
    payload_dispatcher: &Dispatcher,
    era: Era,
) where
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
    UndeliveredPayloads: ExactSizeIterator<Item = DataPayload> + Send,
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
        if let DataPayload::BlockProposal(encoded_proposal) = &payload {
            tracing::debug!(
                target: LOG_TARGET,
                event = "blend_proposal_fallback_broadcast",
                timestamp_unix_ms = crate::message::proposal_diagnostic_timestamp_unix_ms(),
                proposal_digest = %crate::message::proposal_diagnostic_digest_from_encoded(encoded_proposal),
                "Blend delivery deadline expired; broadcasting original block proposal directly"
            );
        }
        metrics::data_payload_bypassed_blend(payload.payload_type());
        payload_dispatcher.dispatch(payload, era)
    }))
    .await;
}
