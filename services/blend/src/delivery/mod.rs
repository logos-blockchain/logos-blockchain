use futures::{Stream, StreamExt as _, future::join_all};

pub use self::failure_detection::FailureDetector;
use crate::{LOG_TARGET, core::network::NetworkAdapter, message::NetworkMessage, metrics};

mod failure_detection;

#[cfg(test)]
pub mod test_utils;

/// The payloads whose deadline has just passed, from a sender that is watching
/// for them.
///
/// A sender that is not — the operator turned the fallback off — waits forever
/// instead, which leaves the `select!` branch this feeds permanently idle
/// rather than firing.
pub async fn next_undelivered_messages<Detection, BroadcastSettings>(
    failure_detection: Option<&mut Detection>,
) -> Option<Vec<NetworkMessage<BroadcastSettings>>>
where
    Detection: Stream<Item = Vec<NetworkMessage<BroadcastSettings>>> + Unpin + Send,
{
    match failure_detection {
        Some(failure_detection) => failure_detection.next().await,
        None => core::future::pending().await,
    }
}

/// Broadcasts, in the clear, the payloads the Blend network did not deliver
/// within the delivery deadline.
pub async fn broadcast_undelivered_messages<NetAdapter, UndeliveredPayloads, RuntimeServiceId>(
    undelivered: UndeliveredPayloads,
    network_adapter: &NetAdapter,
) where
    NetAdapter: NetworkAdapter<RuntimeServiceId> + Sync,
    UndeliveredPayloads:
        ExactSizeIterator<Item = NetworkMessage<NetAdapter::BroadcastSettings>> + Send,
{
    // TODO: Once we switch to a well-defined API for blending payloads, we can and
    // should show the relevant details for each payload that failed. E.g., for
    // blocks we could log the block ID.
    tracing::warn!(
        target: LOG_TARGET,
        "The Blend network did not deliver {} locally-originated payload(s) within the delivery deadline; broadcasting them directly.",
        undelivered.len()
    );
    join_all(undelivered.into_iter().map(
        |NetworkMessage {
             message,
             broadcast_settings,
         }| {
            metrics::message_bypassed_blend();
            network_adapter.broadcast(message, broadcast_settings)
        },
    ))
    .await;
}
