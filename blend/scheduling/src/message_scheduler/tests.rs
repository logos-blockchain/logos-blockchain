use core::{
    num::NonZeroU64,
    task::{Context, Poll},
};
use std::collections::HashSet;

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
        // Round `1` scheduled, tick will yield round `0`.
        SessionCoverTraffic::with_test_values(
            Box::new(iter(rounds)),
            HashSet::from_iter([1u128.into()]),
            rng.clone(),
            0,
        ),
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
        // Round `1` scheduled, tick will yield round `0`.
        SessionCoverTraffic::with_test_values(
            Box::new(iter(rounds)),
            HashSet::from_iter([1u128.into()]),
            rng.clone(),
            0,
        ),
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
        // Round `0` scheduled, tick will yield round `0`.
        SessionCoverTraffic::with_test_values(
            Box::new(iter(rounds)),
            HashSet::from_iter([0u128.into()]),
            rng.clone(),
            0,
        ),
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
        // Round `1` scheduled, tick will yield round `0`.
        SessionCoverTraffic::with_test_values(
            Box::new(iter(rounds)),
            HashSet::from_iter([1u128.into()]),
            rng.clone(),
            0,
        ),
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
        // Round `0` scheduled, tick will yield round `0`.
        SessionCoverTraffic::with_test_values(
            Box::new(iter(rounds)),
            HashSet::from_iter([0u128.into()]),
            rng.clone(),
            0,
        ),
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
        // Round `1` and `2` scheduled, tick will yield round `0` then round `1`, round `2`, then
        // round `3`.
        SessionCoverTraffic::with_test_values(
            Box::new(iter(rounds)),
            HashSet::from_iter([1u128.into(), 2u128.into()]),
            rng.clone(),
            0,
        ),
        // Round `3` scheduled, tick will yield round `0` then round `1`, round `2`, then round
        // `3`.
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

    // Poll for round `0`, which should return `Pending`.
    assert_eq!(scheduler.poll_next_unpin(&mut cx), Poll::Pending);

    // Poll for round `1`, which should return a cover message.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![],
            release_type: Some(RoundReleaseType::OnlyCoverMessage)
        }))
    );
    assert!(scheduler.data_messages.is_empty());

    scheduler.queue_data_message(3);

    // Poll for round `2`, which should skip the cover message (although it's
    // scheduled) and should only return the queued data message instead.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![3],
            release_type: None
        }))
    );
    assert!(scheduler.data_messages.is_empty());

    // Poll for round `3`, which should return the processed messages and the queued
    // data message.
    assert_eq!(
        scheduler.poll_next_unpin(&mut cx),
        Poll::Ready(Some(RoundInfo {
            data_messages: vec![],
            release_type: Some(RoundReleaseType::OnlyProcessedMessages(vec![()]))
        }))
    );
    assert!(scheduler.data_messages.is_empty());
}
