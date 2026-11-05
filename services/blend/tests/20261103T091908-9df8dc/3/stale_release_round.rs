//! The deadline must start in the release round, even when another event-loop
//! branch kept the detector from being polled before release.
use std::{num::NonZeroU64, time::Duration};

use futures::{StreamExt as _, stream};
use tokio::time::Instant;

use crate::{
    delivery::FailureDetector,
    message::DataPayload,
    test_utils::dispatcher::TestPayloadDispatcher,
};

#[tokio::test(start_paused = true)]
async fn a_busy_sender_still_waits_tm_from_the_actual_release_round() {
    let mut detector = FailureDetector::new(
        NonZeroU64::new(6).unwrap(),
        NonZeroU64::new(1).unwrap(),
        stream::pending().boxed(),
    );
    let (dispatcher, mut channel) = TestPayloadDispatcher::new();

    // Model an awaited send or another service branch occupying three rounds.
    // The detector has not been polled while this branch is executing.
    tokio::time::advance(Duration::from_secs(3)).await;
    let released_at = Instant::now();
    let payload = DataPayload::BlockProposal(b"still traversing Blend".to_vec());
    detector.mark_payload_as_blended(payload.clone());

    // Use production expiry and direct-broadcast code. The observer stays open
    // but has not yet seen the payload; delivery remains possible until T_M.
    tokio::time::timeout(
        Duration::from_secs(10),
        detector.drain_pending_message_queue::<_, ()>(&dispatcher),
    )
    .await
    .expect("the detector must eventually drain");
    assert_eq!(channel.dispatched.try_recv().unwrap(), payload);
    assert!(
        released_at.elapsed() >= Duration::from_secs(6),
        "direct broadcast occurred {:?} after release; T_M is six rounds, counted from the release round",
        released_at.elapsed()
    );
}
