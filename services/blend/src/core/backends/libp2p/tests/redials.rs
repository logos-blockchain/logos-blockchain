use core::time::Duration;

use lb_blend::scheduling::membership::Membership;
use lb_core::crypto::ZkHash;
use lb_groth16::Field as _;
use lb_libp2p::{Protocol, SwarmEvent};
use libp2p::{Multiaddr, PeerId};
use test_log::test;
use tokio::{select, time, time::sleep};

use crate::core::backends::{
    SessionInfo,
    libp2p::{
        core_swarm_test_utils::{SwarmExt as _, new_nodes_with_empty_address, update_nodes},
        swarm::BlendSwarmMessage,
        tests::utils::{BlendBehaviourBuilder, SwarmBuilder, TestSwarm},
    },
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

/// When a new session rotation occurs, pending backoff retries should be
/// discarded along with ongoing dials.
#[test(tokio::test)]
async fn core_session_rotation_clears_pending_retries() {
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

    // Trigger a new session via the swarm message channel.
    let new_session_info = SessionInfo {
        membership: Membership::new_without_local(&[]),
        session_number: 2,
        core_public_inputs: lb_blend::proofs::quota::inputs::prove::public::CoreInputs {
            quota: 1,
            zk_root: ZkHash::ZERO,
        },
    };
    swarm_message_sender
        .send(BlendSwarmMessage::StartNewSession(new_session_info))
        .await
        .unwrap();
    dialing_swarm.poll_next().await;

    // Session rotation should have cleared both ongoing dials and pending retries.
    assert!(dialing_swarm.ongoing_dials().is_empty());
    assert_eq!(dialing_swarm.pending_retries_count(), 0);
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
