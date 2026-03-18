use core::{
    num::NonZeroU64,
    task::{Context, Poll},
};

use futures::{StreamExt as _, task::noop_waker_ref};
use lb_utils::blake_rng::BlakeRng;
use rand::SeedableRng as _;
use tokio_stream::iter;

use crate::{
    cover_traffic::SessionCoverTraffic,
    message_scheduler::{
        SessionMessageScheduler,
        round_info::{Round, RoundInfo, RoundReleaseType},
    },
    release_delayer::SessionProcessedMessageDelayer,
};

#[tokio::test]
async fn no_substream_ready_and_no_data_messages() {
    let rng = BlakeRng::from_entropy();
    let rounds = [Round::from(0)];
    let mut scheduler = SessionMessageScheduler::<_, (), ()>::with_test_values(
        // No cover messages to emit, tick will yield round `0`.
        SessionCoverTraffic::with_test_values(Box::new(iter(rounds)), 0, 1.into(), rng.clone(), 0),
        // Round `1` scheduled, tick will yield round `0`.
        SessionProcessedMessageDelayer::with_test_values(
            NonZeroU64::try_from(1).unwrap(),
            1u128.into(),
            rng,
            Box::new(iter(rounds)),
            vec![],
        ),
        // Round clock (same as above)
        Box::new(iter(rounds)),
        vec![],
    );
    let mut cx = Context::from_waker(noop_waker_ref());

    // We poll for round 0, which returns `Pending`, as per the default scheduler
    // configuration.
    assert!(scheduler.poll_next_unpin(&mut cx).is_pending());
}

#[tokio::test]
async fn no_substream_ready_with_data_messages() {
    let rng = BlakeRng::from_entropy();
    let rounds = [Round::from(0)];
    let mut scheduler = SessionMessageScheduler::<_, (), u32>::with_test_values(
        // No cover messages to emit, tick will yield round `0`.
        SessionCoverTraffic::with_test_values(Box::new(iter(rounds)), 0, 1.into(), rng.clone(), 0),
        // Round `1` scheduled, tick will yield round `0`.
        SessionProcessedMessageDelayer::with_test_values(
            NonZeroU64::try_from(1).unwrap(),
            1u128.into(),
            rng,
            Box::new(iter(rounds)),
            vec![],
        ),
        // Round clock (same as above)
        Box::new(iter(rounds)),
        vec![1, 2],
    );
    let mut cx = Context::from_waker(noop_waker_ref());

    // We poll for round 0, which returns `Ready` since we have data messages to
    // return.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![1, 2],
            release_type: None
        }))
    );
    // We test that the released data messages have been removed from the queue.
    assert!(scheduler.data_messages.is_empty());
}

#[tokio::test]
async fn cover_traffic_substream_ready() {
    let rng = BlakeRng::from_entropy();
    let rounds = [Round::from(0)];
    let mut scheduler = SessionMessageScheduler::<_, (), u32>::with_test_values(
        // 1 cover message over 1 round: guaranteed emission.
        SessionCoverTraffic::with_test_values(Box::new(iter(rounds)), 1, 1.into(), rng.clone(), 0),
        // Round `1` scheduled, tick will yield round `0`.
        SessionProcessedMessageDelayer::with_test_values(
            NonZeroU64::try_from(1).unwrap(),
            1u128.into(),
            rng,
            Box::new(iter(rounds)),
            vec![],
        ),
        // Round clock (same as above)
        Box::new(iter(rounds)),
        vec![1],
    );
    let mut cx = Context::from_waker(noop_waker_ref());

    // Poll for round 0, which should return a cover message.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![1],
            release_type: Some(RoundReleaseType::OnlyCoverMessage)
        }))
    );
}

#[tokio::test]
async fn release_delayer_substream_ready() {
    let rng = BlakeRng::from_entropy();
    let rounds = [Round::from(0)];
    let mut scheduler = SessionMessageScheduler::<_, u32, u32>::with_test_values(
        // No cover messages to emit.
        SessionCoverTraffic::with_test_values(Box::new(iter(rounds)), 0, 1.into(), rng.clone(), 0),
        // Round `0` scheduled, tick will yield round `0`.
        SessionProcessedMessageDelayer::with_test_values(
            NonZeroU64::try_from(1).unwrap(),
            0u128.into(),
            rng,
            Box::new(iter(rounds)),
            vec![1],
        ),
        // Round clock (same as above)
        Box::new(iter(rounds)),
        vec![2],
    );
    let mut cx = Context::from_waker(noop_waker_ref());

    // Poll for round 0, which should return the processed messages.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![2],
            release_type: Some(RoundReleaseType::OnlyProcessedMessages(vec![1]))
        }))
    );
}

#[tokio::test]
async fn both_substreams_ready() {
    let rng = BlakeRng::from_entropy();
    let rounds = [Round::from(0)];
    let mut scheduler = SessionMessageScheduler::<_, u32, ()>::with_test_values(
        // 1 cover message over 1 round: guaranteed emission.
        SessionCoverTraffic::with_test_values(Box::new(iter(rounds)), 1, 1.into(), rng.clone(), 0),
        // Round `0` scheduled, tick will yield round `0`.
        SessionProcessedMessageDelayer::with_test_values(
            NonZeroU64::try_from(1).unwrap(),
            0u128.into(),
            rng,
            Box::new(iter(rounds)),
            vec![1],
        ),
        // Round clock (same as above)
        Box::new(iter(rounds)),
        vec![],
    );
    let mut cx = Context::from_waker(noop_waker_ref());

    // Poll for round 0, which should return the processed messages and a cover
    // message.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![],
            release_type: Some(RoundReleaseType::ProcessedAndCoverMessages(vec![1]))
        }))
    );
}

#[tokio::test]
async fn round_change() {
    let rng = BlakeRng::from_entropy();
    let rounds = [
        Round::from(0),
        Round::from(1),
        Round::from(2),
        Round::from(3),
    ];
    let mut scheduler = SessionMessageScheduler::<_, (), u32>::with_test_values(
        // 2 cover messages over 2 remaining rounds: every round is guaranteed to be a
        // release round. After the first emission, a data message will cause the second
        // release to be skipped (threshold = 1/1 = 1.0).
        SessionCoverTraffic::with_test_values(Box::new(iter(rounds)), 2, 2.into(), rng.clone(), 0),
        // Round `3` scheduled, tick will yield rounds `0` through `3`.
        SessionProcessedMessageDelayer::with_test_values(
            NonZeroU64::try_from(1).unwrap(),
            3u128.into(),
            rng,
            Box::new(iter(rounds)),
            vec![()],
        ),
        // Round clock (same as above)
        Box::new(iter(rounds)),
        vec![],
    );
    let mut cx = Context::from_waker(noop_waker_ref());

    // Poll for round `0`: cover traffic emits (prob = 2/2 = 1.0), no processed
    // messages, no data messages.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![],
            release_type: Some(RoundReleaseType::OnlyCoverMessage)
        }))
    );
    assert!(scheduler.data_messages.is_empty());

    scheduler.queue_data_message(3);

    // Poll for round `1`: cover traffic release round (prob = 1/1 = 1.0) but
    // skipped due to unprocessed data message (threshold = 1/1 = 1.0, always
    // skips). Returns only the queued data message.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![3],
            release_type: None
        }))
    );
    assert!(scheduler.data_messages.is_empty());

    // Poll for round `2`: no cover (remaining_messages exhausted), no processed
    // messages, no data -> Pending.
    assert_eq!(scheduler.poll_next_unpin(&mut cx), Poll::Pending);

    // Poll for round `3`: processed messages released.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![],
            release_type: Some(RoundReleaseType::OnlyProcessedMessages(vec![()]))
        }))
    );
    assert!(scheduler.data_messages.is_empty());
}
