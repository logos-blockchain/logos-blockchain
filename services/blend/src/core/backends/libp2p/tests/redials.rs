use core::time::Duration;

use lb_blend::{
    crypto::ZkHash, proofs::quota::inputs::prove::public::CoreInputs,
    scheduling::membership::Membership,
};
use lb_groth16::Field as _;
use lb_libp2p::{Protocol, SwarmEvent};
use libp2p::{Multiaddr, PeerId};
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::backends::{
    SessionInfo,
    libp2p::{
        core_swarm_test_utils::{SwarmExt as _, new_nodes_with_empty_address, update_nodes},
        swarm::BlendSwarmMessage,
        tests::utils::{BlendBehaviourBuilder, SwarmBuilder, TestSwarm},
    },
};

/// With exponential backoff the flow is:
/// 1. First dial (attempt 1) → immediate `OutgoingConnectionError`.
/// 2. `schedule_retry` moves the peer to the backoff queue (`retry_count` = 1).
/// 3. After the backoff delay, `execute_retry` re-inserts the peer into
///    `active` and dials again (attempt 2).
/// 4. Repeat until `max_dial_attempts` (3) is exhausted.
#[test(tokio::test)]
async fn core_redial_same_peer() {
    tokio::time::pause();

    let (mut identities, peer_ids) = new_nodes_with_empty_address(1);
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let random_peer_id = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();
    dialing_swarm.dial_peer_at_addr(random_peer_id, empty_multiaddr.clone());

    // --- Attempt 1: peer is in active dials ---
    let dial_attempt_1 = dialing_swarm.ongoing_dials().get(&random_peer_id).unwrap();
    assert_eq!(dial_attempt_1.address, empty_multiaddr);
    assert_eq!(dial_attempt_1.attempt_number, 1.try_into().unwrap());

    // Poll until the first dial fails.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    // After failure, not in active (moved to backoff queue).
    assert!(dialing_swarm.ongoing_dials().get(&random_peer_id).is_none());
    assert_eq!(dialing_swarm.ongoing_dials().retry_count(), 1);

    // --- Attempt 2: advance time past the first backoff (2s) ---
    tokio::time::advance(Duration::from_secs(3)).await;
    // Poll so the retry fires and the second dial is placed.
    dialing_swarm.poll_next().await;

    let dial_attempt_2 = dialing_swarm.ongoing_dials().get(&random_peer_id).unwrap();
    assert_eq!(dial_attempt_2.address, empty_multiaddr);
    assert_eq!(dial_attempt_2.attempt_number, 2.try_into().unwrap());

    // Poll until second dial fails.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    // After second failure, peer again in backoff queue.
    assert!(dialing_swarm.ongoing_dials().get(&random_peer_id).is_none());
    assert_eq!(dialing_swarm.ongoing_dials().retry_count(), 1);

    // --- Attempt 3: advance past second backoff (4s), which hits max ---
    tokio::time::advance(Duration::from_secs(5)).await;
    dialing_swarm.poll_next().await;

    let dial_attempt_3 = dialing_swarm.ongoing_dials().get(&random_peer_id).unwrap();
    assert_eq!(dial_attempt_3.address, empty_multiaddr);
    assert_eq!(dial_attempt_3.attempt_number, 3.try_into().unwrap());

    // Poll until third (final) dial fails.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    // Max attempts exhausted; peer removed entirely, no pending retries.
    assert!(dialing_swarm.ongoing_dials().is_empty());
    assert_eq!(dialing_swarm.ongoing_dials().retry_count(), 0);
}

#[test(tokio::test)]
async fn core_redial_different_peer_after_redial_limit() {
    tokio::time::pause();

    let (mut identities, mut nodes) = new_nodes_with_empty_address(2);
    let TestSwarm {
        swarm: mut listening_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (listening_node, _) = listening_swarm
        .listen_and_return_membership_entry(None)
        .await;
    update_nodes(&mut nodes, &listening_node.id, listening_node.address);

    // Build dialing swarm with the listening info of the listening swarm.
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let dialing_peer_id = *dialing_swarm.local_peer_id();

    // Dial a random peer on a random address, which should fail after the maximum
    // number of attempts, after which the dialing swarm should connect to the
    // listening swarm.
    dialing_swarm.dial_peer_at_addr(PeerId::random(), Protocol::Memory(0).into());

    // Advance time enough for all backoff retries to complete (2s + 4s + margin).
    loop {
        select! {
            () = sleep(Duration::from_secs(10)) => {
                break;
            }
            () = dialing_swarm.poll_next() => {}
            () = listening_swarm.poll_next() => {}
        }
    }

    assert!(dialing_swarm.ongoing_dials().is_empty());
    assert!(
        dialing_swarm
            .behaviour()
            .blend
            .with_core()
            .negotiated_peers()
            .contains_key(&listening_node.id)
    );
    assert_eq!(
        dialing_swarm
            .behaviour()
            .blend
            .with_core()
            .num_healthy_peers(),
        1
    );
    assert!(
        listening_swarm
            .behaviour()
            .blend
            .with_core()
            .negotiated_peers()
            .contains_key(&dialing_peer_id)
    );
}

/// Verify that the backoff delay is actually respected: a retry scheduled
/// after the first failure should NOT fire before the backoff time elapses.
#[test(tokio::test)]
async fn core_backoff_delay_is_respected() {
    tokio::time::pause();

    let (mut identities, peer_ids) = new_nodes_with_empty_address(1);
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let random_peer_id = PeerId::random();
    dialing_swarm.dial_peer_at_addr(random_peer_id, Protocol::Memory(0).into());

    // Poll until the first dial fails.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    // Retry is pending; active map should be empty.
    assert!(dialing_swarm.ongoing_dials().get(&random_peer_id).is_none());
    assert_eq!(dialing_swarm.ongoing_dials().retry_count(), 1);

    // Advance time by only 1s; the first retry backoff is 2s, so the
    // retry must NOT have fired yet.
    tokio::time::advance(Duration::from_secs(1)).await;

    // Drive the reactor so any ready futures would fire.
    select! {
        () = sleep(Duration::from_millis(10)) => {}
        () = dialing_swarm.poll_next() => {}
    }

    // Still pending - no active dial re-inserted.
    assert!(dialing_swarm.ongoing_dials().get(&random_peer_id).is_none());
    assert_eq!(dialing_swarm.ongoing_dials().retry_count(), 1);

    // Now advance past the 2s backoff.
    tokio::time::advance(Duration::from_secs(2)).await;
    dialing_swarm.poll_next().await;

    // Retry should have fired: peer is back in active dials at attempt 2.
    let attempt = dialing_swarm.ongoing_dials().get(&random_peer_id).unwrap();
    assert_eq!(attempt.attempt_number, 2.try_into().unwrap());
    assert_eq!(dialing_swarm.ongoing_dials().retry_count(), 0);
}

/// When a new session clears the dial map, pending backoff retries should
/// also be discarded.
#[test(tokio::test)]
async fn core_session_rotation_clears_pending_retries() {
    tokio::time::pause();

    let (mut identities, peer_ids) = new_nodes_with_empty_address(1);
    let TestSwarm {
        swarm: mut dialing_swarm,
        swarm_message_sender,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let random_peer_id = PeerId::random();
    dialing_swarm.dial_peer_at_addr(random_peer_id, Protocol::Memory(0).into());

    // Poll until first dial fails -> retry queued.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;
    assert_eq!(dialing_swarm.ongoing_dials().retry_count(), 1);

    // Trigger a new session via the swarm message channel.
    let new_session_info = SessionInfo {
        membership: Membership::new_without_local(&[]),
        session_number: 2,
        core_public_inputs: CoreInputs {
            quota: 1,
            zk_root: ZkHash::ZERO,
        },
    };
    swarm_message_sender
        .send(BlendSwarmMessage::StartNewSession(new_session_info))
        .await
        .unwrap();
    dialing_swarm.poll_next().await;

    // Session rotation should have cleared both active and retries.
    assert!(dialing_swarm.ongoing_dials().is_empty());
    assert_eq!(dialing_swarm.ongoing_dials().retry_count(), 0);
}
