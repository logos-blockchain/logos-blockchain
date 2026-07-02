use core::time::Duration;
use std::collections::HashSet;

use lb_blend::scheduling::membership::Membership;
use lb_libp2p::{Protocol, SwarmEvent};
use libp2p::{Multiaddr, PeerId};
use test_log::test;
use tokio::{
    select,
    time::{self, sleep, sleep_until},
};

use crate::core::backends::libp2p::{
    core_swarm_test_utils::{SwarmExt as _, new_nodes_with_empty_address, update_nodes},
    swarm::BlendSwarmMessage,
    tests::utils::{BlendBehaviourBuilder, SwarmBuilder, TestSwarm, build_membership},
};

#[test(tokio::test)]
async fn core_redial_same_peer() {
    let (mut identities, peer_ids) = new_nodes_with_empty_address(1);
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let random_peer_id = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();
    dialing_swarm.dial_peer_at_addr(random_peer_id, empty_multiaddr.clone());

    // After dial, the first attempt should be in ongoing_dials.
    let dial_attempt_1 = dialing_swarm.ongoing_dials().get(&random_peer_id).unwrap();
    assert_eq!(*dial_attempt_1.address(), empty_multiaddr);
    assert_eq!(dial_attempt_1.attempt_number(), 1.try_into().unwrap());

    // Poll through all 3 dial attempts (each fails with OutgoingConnectionError).
    // Between errors, schedule_retry removes the entry from ongoing_dials and
    // schedules a delayed retry, so we cannot check intermediate ongoing_dials
    // state.
    for _ in 0..3 {
        dialing_swarm
            .poll_next_until(|event| {
                let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                    return false;
                };
                *peer_id == Some(random_peer_id)
            })
            .await;
        // Before retrying, the failed peer should have been removed from ongoing_dials.
        assert!(!dialing_swarm.ongoing_dials().contains_key(&random_peer_id));
    }

    // All attempts exhausted. Storage map should be cleared up, and since there
    // is no other peer, no new peer is dialed.
    assert!(dialing_swarm.ongoing_dials().is_empty());
}

#[test(tokio::test)]
async fn core_redial_different_peer_after_redial_limit() {
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

    // Allow enough time for backoff retries to complete (2s + 4s + margin).
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

/// Verifies that retries use exponential backoff by measuring the elapsed time
/// between consecutive connection errors.
#[test(tokio::test)]
async fn core_redial_uses_exponential_backoff() {
    let (mut identities, peer_ids) = new_nodes_with_empty_address(1);
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let random_peer_id = PeerId::random();
    dialing_swarm.dial_peer_at_addr(random_peer_id, Protocol::Memory(0).into());

    // Wait for the first error (no backoff on the initial dial).
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    // After the error, the entry should be removed from ongoing_dials and a
    // retry should be pending.
    assert!(dialing_swarm.ongoing_dials().get(&random_peer_id).is_none());
    assert_eq!(dialing_swarm.pending_retries_count(), 1);

    // Measure the delay until the second error. With exponential backoff, the
    // retry (attempt 2) is delayed by 2^1 = 2 seconds.
    let before_second_error = time::Instant::now();
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;
    let first_backoff = before_second_error.elapsed();
    assert!(first_backoff >= Duration::from_secs(2),);

    // Measure the delay until the third error. The retry (attempt 3) should be
    // delayed by 2^2 = 4 seconds.
    let before_third_error = time::Instant::now();
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;
    let second_backoff = before_third_error.elapsed();
    assert!(second_backoff >= Duration::from_secs(4),);
}

/// Verifies that when a peer fails all dial attempts, it is added to the failed
/// peers set, preventing it from being chosen again on the next attempt.
#[test(tokio::test)]
async fn core_remembers_failed_peers_across_retries() {
    // Create 3 membership nodes: 1 local (the dialing swarm) + 2 remote
    // unreachable.
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        // Use max_dial_attempts=1 to avoid backoff delays.
        .with_max_dial_attempts(1.try_into().unwrap())
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    // Manually dial one of the unreachable peers.
    let unreachable_peer_1 = nodes[1].id;
    let unreachable_peer_2 = nodes[2].id;
    dialing_swarm.dial_peer_at_addr(unreachable_peer_1, Protocol::Memory(0).into());

    // The first dial has no failed peers memory.
    assert_eq!(
        dialing_swarm.failed_peers_for(&unreachable_peer_1),
        Some(&HashSet::new()),
    );

    // Let the first peer fail its single dial attempt.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(unreachable_peer_1)
        })
        .await;

    // After failure, check_and_dial_new_peers should have picked the second peer,
    // and the second peer's dial attempt should carry the first peer in its
    // failed_peers set.
    assert!(
        dialing_swarm
            .ongoing_dials()
            .contains_key(&unreachable_peer_2),
        "Second peer should be dialed after first peer failed"
    );
    assert_eq!(
        dialing_swarm.failed_peers_for(&unreachable_peer_2),
        Some(&HashSet::from([unreachable_peer_1])),
    );
}

/// Verifies that when all peers have been tried and failed in a cycle, the node
/// does not immediately re-dial the whole membership (which would spin at
/// event-loop speed against peers that keep rejecting us). Instead it schedules
/// a single delayed retry from scratch; when the cooldown elapses,
/// `check_and_dial_new_peers` re-dials the membership with a cleared
/// failed-peers set.
#[test(tokio::test)]
async fn core_schedules_delayed_retry_when_all_peers_exhausted() {
    // Create 3 membership nodes: 1 local + 2 remote unreachable.
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .with_max_dial_attempts(1.try_into().unwrap())
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let unreachable_peer_1 = nodes[1].id;
    dialing_swarm.dial_peer_at_addr(unreachable_peer_1, Protocol::Memory(0).into());

    // Fail the first peer.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(unreachable_peer_1)
        })
        .await;

    // The second peer should now be being dialed.
    let second_peer = *dialing_swarm
        .ongoing_dials()
        .keys()
        .next()
        .expect("should have an ongoing dial");
    assert_ne!(unreachable_peer_1, second_peer);

    // Fail the second peer.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(second_peer)
        })
        .await;

    // Both peers have been tried this cycle. Instead of an immediate re-dial, a
    // single delayed retry-from-scratch should be scheduled, with no dial in
    // flight in the meantime.
    assert!(
        dialing_swarm.ongoing_dials().is_empty(),
        "No immediate re-dial should happen; the retry from scratch is delayed"
    );
    assert!(
        dialing_swarm.has_pending_full_membership_retry(),
        "A delayed retry-from-scratch should be scheduled after all peers have been tried"
    );
}

/// When a new epoch rotation occurs, pending backoff retries should be
/// discarded along with ongoing dials.
#[test(tokio::test)]
async fn core_epoch_rotation_clears_pending_retries() {
    let (mut identities, peer_ids) = new_nodes_with_empty_address(1);
    let TestSwarm {
        swarm: mut dialing_swarm,
        swarm_message_sender,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let random_peer_id = PeerId::random();
    dialing_swarm.dial_peer_at_addr(random_peer_id, Protocol::Memory(0).into());

    // Poll until the first dial fails -> retry queued.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;
    assert_eq!(dialing_swarm.pending_retries_count(), 1);

    // Trigger a new epoch via the swarm message channel.
    let new_epoch_info = (Membership::new_without_local(&[]), 2.into());
    swarm_message_sender
        .send(BlendSwarmMessage::StartNewEpoch(new_epoch_info))
        .await
        .unwrap();
    dialing_swarm.poll_next().await;

    // Epoch rotation should have cleared both ongoing dials and pending retries.
    assert!(dialing_swarm.ongoing_dials().is_empty());
    assert_eq!(dialing_swarm.pending_retries_count(), 0);
}

/// Reproduces the peering-degree maintenance bug at an epoch boundary.
///
/// Epochs are independent: when a node enters a new epoch it rebuilds its peer
/// set from scratch and should keep dialing until it reaches the *minimum*
/// peering degree. Here the new epoch's membership has one reachable peer and
/// two unreachable ones, with a minimum peering degree of 2. The reachable peer
/// negotiates (healthy = 1), the two unreachable peers exhaust their dial
/// attempts, and the node then stops dialing entirely — stranded one peer below
/// the configured minimum until the *next* epoch.
///
/// Root cause: in `dial_random_peers_except`, the "retry everything from
/// scratch" reset is gated on `except.len() == membership.size() - 1`, but a
/// successfully negotiated peer is excluded via the separate `negotiated_peers`
/// set rather than via `except`. So once any peer negotiates, `except` can only
/// ever reach `size - 2`, the reset never fires, and the node gives up below
/// the minimum.
#[test(tokio::test)]
async fn core_does_not_give_up_below_minimum_peering_degree() {
    let min_peering_degree = 2;

    // 4 membership nodes: [0] reachable listener, [1] local dialer, [2]/[3]
    // unreachable.
    let (mut identities, mut nodes) = new_nodes_with_empty_address(4);

    // The single reachable peer: a real listening swarm.
    let TestSwarm {
        swarm: mut reachable_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes).build(|id, membership| {
        BlendBehaviourBuilder::new(id, membership)
            .with_peering_degree(min_peering_degree..=3)
            .build()
    });
    let (reachable_node, _) = reachable_swarm
        .listen_and_return_membership_entry(None)
        .await;
    update_nodes(&mut nodes, &reachable_node.id, reachable_node.address);

    // Two unreachable peers: dialable memory addresses with no listener behind
    // them, so every dial attempt fails with a transport error.
    let unreachable_addr: Multiaddr = Protocol::Memory(0).into();
    let (unreachable_1, unreachable_2) = (nodes[2].id, nodes[3].id);
    update_nodes(&mut nodes, &unreachable_1, unreachable_addr.clone());
    update_nodes(&mut nodes, &unreachable_2, unreachable_addr);

    // The node under test.
    let TestSwarm {
        swarm: mut dialing_swarm,
        swarm_message_sender,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .with_max_dial_attempts(1.try_into().unwrap())
        .build(|id, membership| {
            BlendBehaviourBuilder::new(id, membership)
                .with_peering_degree(min_peering_degree..=3)
                .build()
        });

    // Enter a new epoch: this triggers the new-epoch peering logic, which dials
    // peers to reach the minimum degree.
    let new_membership = build_membership(&nodes, Some(*dialing_swarm.local_peer_id()));
    swarm_message_sender
        .send(BlendSwarmMessage::StartNewEpoch((new_membership, 2.into())))
        .await
        .unwrap();

    // Drive both swarms for a fixed window: the reachable peer connects and the
    // two unreachable peers fail. A node below the minimum peering degree keeps
    // retrying, so the swarm never goes quiet — we therefore run until a fixed
    // deadline (NOT until quiescence, which would never arrive) and then inspect
    // the steady state. The deadline is computed once; recreating `sleep` each
    // iteration would reset the timer and loop forever.
    let deadline = time::Instant::now() + Duration::from_secs(3);
    loop {
        select! {
            () = sleep_until(deadline) => break,
            () = dialing_swarm.poll_next() => {}
            () = reachable_swarm.poll_next() => {}
        }
    }

    let healthy = dialing_swarm
        .behaviour()
        .blend
        .with_core()
        .num_healthy_peers();
    let ongoing = dialing_swarm.ongoing_dials().len();
    let pending = dialing_swarm.pending_retries_count();
    let full_retry = dialing_swarm.has_pending_full_membership_retry();

    // Sanity: the reachable peer connected, so we are genuinely one short of the
    // minimum (not simply failing to connect to anyone).
    assert_eq!(
        healthy, 1,
        "setup precondition: the single reachable peer should have negotiated"
    );

    // Regression guard: a node below the minimum peering degree must not give up
    // — it must still be trying to reach the minimum, whether via an ongoing
    // dial, a pending per-peer retry, or a scheduled retry-from-scratch. Before
    // the fix it ended with none of these, stranded at degree 1 < 2 until the
    // next epoch.
    assert!(
        healthy >= min_peering_degree || ongoing > 0 || pending > 0 || full_retry,
        "node gave up below the minimum peering degree: healthy={healthy} < {min_peering_degree}, \
         ongoing_dials={ongoing}, pending_retries={pending}, full_membership_retry={full_retry} — \
         it will stay stranded until the next epoch"
    );
}

/// The periodic peering-degree maintenance task must dial peers to reach the
/// minimum degree even when nothing else triggers a dial — no `StartNewEpoch`,
/// no dial failure, no disconnection. Here the node starts quiescent and below
/// degree (`new_test` performs no initial dial and we send no message), so the
/// ONLY thing that can make it connect to the reachable peer is the maintenance
/// tick.
#[test(tokio::test)]
async fn core_maintenance_task_dials_to_maintain_peering_degree() {
    let (mut identities, mut nodes) = new_nodes_with_empty_address(2);

    // The reachable peer: a real listening swarm.
    let TestSwarm {
        swarm: mut reachable_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (reachable_node, _) = reachable_swarm
        .listen_and_return_membership_entry(None)
        .await;
    update_nodes(&mut nodes, &reachable_node.id, reachable_node.address);

    // The node under test: a short maintenance interval. No initial dial is
    // triggered (new_test does not dial, and we send no StartNewEpoch).
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .with_peering_degree_check_interval(time::interval(Duration::from_millis(200)))
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    // Precondition: the node is idle and below the (default) minimum degree of 1.
    assert!(dialing_swarm.ongoing_dials().is_empty());
    assert_eq!(
        dialing_swarm
            .behaviour()
            .blend
            .with_core()
            .num_healthy_peers(),
        0
    );

    // Drive both swarms until a fixed deadline. The only thing that can initiate a
    // dial is the periodic maintenance tick; once it fires it must connect to the
    // reachable peer.
    let deadline = time::Instant::now() + Duration::from_secs(2);
    loop {
        select! {
            () = sleep_until(deadline) => break,
            () = dialing_swarm.poll_next() => {}
            () = reachable_swarm.poll_next() => {}
        }
    }

    assert!(
        dialing_swarm
            .behaviour()
            .blend
            .with_core()
            .negotiated_peers()
            .contains_key(&reachable_node.id),
        "the maintenance task should have dialed and negotiated with the reachable peer"
    );
}

/// When a retry fires but the peering degree is already satisfied (because
/// another peer connected in the meantime), the retry should be skipped.
#[test(tokio::test)]
async fn core_retry_skipped_when_peering_degree_satisfied() {
    let (mut identities, mut nodes) = new_nodes_with_empty_address(2);

    // First swarm: the one that will listen and successfully connect.
    let TestSwarm {
        swarm: mut listening_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (listening_node, _) = listening_swarm
        .listen_and_return_membership_entry(None)
        .await;
    let listening_node_id = listening_node.id;
    let listening_node_address = listening_node.address.clone();
    update_nodes(&mut nodes, &listening_node.id, listening_node.address);

    // Second swarm: the dialer with knowledge of both peers.
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    // Dial an unreachable peer first. This will fail and schedule a retry.
    let unreachable_peer = PeerId::random();
    dialing_swarm.dial_peer_at_addr(unreachable_peer, Protocol::Memory(0).into());

    // Poll until the first dial fails and a retry is pending.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(unreachable_peer)
        })
        .await;
    assert_eq!(dialing_swarm.pending_retries_count(), 1);

    // Now also dial the listening peer, which should succeed and satisfy the
    // minimum peering degree (1).
    dialing_swarm.dial_peer_at_addr(listening_node_id, listening_node_address);

    // Poll both swarms until the connection is established, then wait for the
    // backoff retry to fire. The retry for the unreachable peer should be
    // skipped because peering degree is already satisfied.
    loop {
        select! {
            () = sleep(Duration::from_secs(5)) => {
                break;
            }
            () = dialing_swarm.poll_next() => {}
            () = listening_swarm.poll_next() => {}
        }
    }

    // The unreachable peer's retry was skipped (peering degree was satisfied),
    // so it should not be re-inserted into ongoing_dials.
    assert!(
        dialing_swarm
            .ongoing_dials()
            .get(&unreachable_peer)
            .is_none()
    );
    // The pending retries queue should have been drained.
    assert_eq!(dialing_swarm.pending_retries_count(), 0);
    // Peering degree should be satisfied.
    assert!(
        dialing_swarm
            .behaviour()
            .blend
            .with_core()
            .num_healthy_peers()
            >= 1
    );
}
