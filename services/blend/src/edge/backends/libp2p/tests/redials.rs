use core::slice::from_ref;
use std::collections::HashSet;

use lb_blend::{
    message::crypto::key_ext::Ed25519SecretKeyExt as _,
    scheduling::membership::{Membership, Node},
};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use lb_libp2p::{Protocol, SwarmEvent};
use libp2p::{Multiaddr, PeerId};
use test_log::test;
use tokio::{spawn, time};

use crate::{
    core::backends::libp2p::core_swarm_test_utils::{
        BlendBehaviourBuilder, SwarmBuilder as CoreSwarmBuilder, SwarmExt as _,
        TestSwarm as CoreTestSwarm, new_nodes_with_empty_address,
    },
    edge::backends::libp2p::tests::utils::{
        SwarmBuilder as EdgeSwarmBuilder, TestSwarm as EdgeTestSwarm,
    },
    test_utils::TestEncapsulatedMessage,
};

#[test(tokio::test)]
async fn edge_redial_same_peer() {
    let random_peer_id = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();

    // Configure swarm with an unreachable member.
    let EdgeTestSwarm { mut swarm, .. } =
        EdgeSwarmBuilder::new(Membership::new_without_local(from_ref(&Node {
            address: empty_multiaddr.clone(),
            id: random_peer_id,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        })))
        .with_max_dial_attempts(3.try_into().unwrap())
        .build();
    let message = TestEncapsulatedMessage::new(b"test-payload");
    swarm.send_message(&message);

    // After send_message, the first dial attempt should be in pending_dials.
    let dial_attempt_1_record = swarm
        .pending_dials()
        .iter()
        .filter(|((peer_id, _), _)| peer_id == &random_peer_id)
        .map(|(_, value)| value)
        .next()
        .unwrap();
    assert_eq!(*dial_attempt_1_record.address(), empty_multiaddr);
    assert_eq!(
        dial_attempt_1_record.attempt_number(),
        1.try_into().unwrap()
    );
    assert_eq!(*dial_attempt_1_record.message(), message.clone());

    // Poll through all 3 dial attempts (each fails with OutgoingConnectionError).
    // Between errors, schedule_retry removes the entry from pending_dials and
    // schedules a delayed retry, so we cannot check intermediate pending_dials
    // state.
    for _ in 0..3 {
        swarm
            .poll_next_until(|event| {
                let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                    return false;
                };
                *peer_id == Some(random_peer_id)
            })
            .await;
    }

    // All attempts exhausted. Since there is only one peer in the membership,
    // the failed peers memory is cleared and the same peer is retried from scratch.
    assert!(
        !swarm.pending_dials().is_empty(),
        "Peer should be retried after failed peers memory is cleared"
    );
    assert_eq!(
        swarm.failed_peers_for(&random_peer_id),
        Some(&HashSet::new()),
        "Failed peers memory should be empty after reset"
    );
}

#[test(tokio::test)]
async fn edge_redial_different_peer_after_redial_limit() {
    let random_peer_id = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();

    let (mut identities, peer_ids) = new_nodes_with_empty_address(1);
    let CoreTestSwarm {
        swarm: mut core_swarm,
        incoming_message_receiver: mut core_swarm_incoming_message_receiver,
        ..
    } = CoreSwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (core_swarm_membership_entry, _) =
        core_swarm.listen_and_return_membership_entry(None).await;

    // We include both the core and the unreachable swarm in the membership.
    let edge_membership = Membership::new_without_local(&[
        core_swarm_membership_entry,
        Node {
            address: empty_multiaddr,
            id: random_peer_id,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        },
    ]);
    // We use max_dial_attempts=1 to avoid backoff delays in this test.
    let EdgeTestSwarm {
        swarm: mut edge_swarm,
        ..
    } = EdgeSwarmBuilder::new(edge_membership)
        .with_max_dial_attempts(1)
        .build();

    let message = TestEncapsulatedMessage::new(b"test-payload");

    // We instruct the swarm to try to dial the unreachable swarm first by excluding
    // the core swarm from the initial set of recipients.
    edge_swarm.send_message_to_anyone_except(*core_swarm.local_peer_id(), &message);

    spawn(async move { core_swarm.run().await });
    spawn(async move { edge_swarm.run().await });

    // Verify the message is anyway received by the core swarm after the maximum
    // number of dial attempts have been performed with the unreachable address.
    let (received_message, received_message_session) =
        core_swarm_incoming_message_receiver.recv().await.unwrap();
    assert_eq!(received_message, message.into_inner().into());
    assert_eq!(received_message_session, 1);
}

/// Verifies that when a peer fails all dial attempts, it is added to the failed
/// peers set, preventing it from being chosen again on the next attempt.
#[test(tokio::test)]
async fn edge_remembers_failed_peers_across_retries() {
    let unreachable_peer_1 = PeerId::random();
    let unreachable_peer_2 = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();

    let membership = Membership::new_without_local(&[
        Node {
            address: empty_multiaddr.clone(),
            id: unreachable_peer_1,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        },
        Node {
            address: empty_multiaddr.clone(),
            id: unreachable_peer_2,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        },
    ]);

    // Use max_dial_attempts=1 and replication_factor=1 so each peer fails
    // immediately and only one peer is chosen at a time.
    let EdgeTestSwarm { mut swarm, .. } = EdgeSwarmBuilder::new(membership)
        .with_max_dial_attempts(1)
        .with_replication_factor(1)
        .build();

    let message = TestEncapsulatedMessage::new(b"test-payload");
    swarm.send_message(&message);

    // Determine which peer was chosen first.
    let first_peer = swarm
        .pending_dials()
        .keys()
        .next()
        .expect("should have a pending dial")
        .0;
    let second_peer = if first_peer == unreachable_peer_1 {
        unreachable_peer_2
    } else {
        unreachable_peer_1
    };

    // The first dial has no failed peers memory.
    assert_eq!(swarm.failed_peers_for(&first_peer), Some(&HashSet::new()),);

    // Let the first peer fail.
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(first_peer)
        })
        .await;

    // The second peer should now be dialed, with the first peer in the failed set.
    assert!(
        swarm
            .pending_dials()
            .keys()
            .any(|(pid, _)| *pid == second_peer),
        "Second peer should be dialed after first peer failed"
    );
    assert_eq!(
        swarm.failed_peers_for(&second_peer),
        Some(&HashSet::from([first_peer])),
    );
}

/// Verifies that when all peers have been tried and failed, the failed peers
/// memory is cleared and peers are retried from scratch.
#[test(tokio::test)]
async fn edge_clears_failed_peers_memory_when_all_exhausted() {
    let unreachable_peer_1 = PeerId::random();
    let unreachable_peer_2 = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();

    let membership = Membership::new_without_local(&[
        Node {
            address: empty_multiaddr.clone(),
            id: unreachable_peer_1,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        },
        Node {
            address: empty_multiaddr.clone(),
            id: unreachable_peer_2,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        },
    ]);

    let EdgeTestSwarm { mut swarm, .. } = EdgeSwarmBuilder::new(membership)
        .with_max_dial_attempts(1)
        .with_replication_factor(1)
        .build();

    let message = TestEncapsulatedMessage::new(b"test-payload");
    swarm.send_message(&message);

    // Fail the first peer.
    let first_peer = swarm.pending_dials().keys().next().unwrap().0;
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(first_peer)
        })
        .await;

    // Fail the second peer.
    let second_peer = swarm.pending_dials().keys().next().unwrap().0;
    assert_ne!(first_peer, second_peer);
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(second_peer)
        })
        .await;

    // Both peers have been tried. The memory should be cleared and a new dial
    // should be attempted (one of the two peers picked again with an empty
    // failed_peers set).
    assert!(
        !swarm.pending_dials().is_empty(),
        "A new dial should be attempted after clearing failed peers memory"
    );
    let retried_peer = swarm.pending_dials().keys().next().unwrap().0;
    assert_eq!(
        swarm.failed_peers_for(&retried_peer),
        Some(&HashSet::new()),
        "Failed peers memory should be cleared after all peers have been tried"
    );
}

/// Verifies that retries use exponential backoff by measuring the elapsed time
/// between consecutive connection errors.
#[test(tokio::test)]
async fn edge_redial_uses_exponential_backoff() {
    let random_peer_id = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();

    // Use max_dial_attempts=3 so we get two backoff intervals to verify:
    // attempt 1 -> fail -> 2s delay -> attempt 2 -> fail -> 4s delay -> attempt 3
    let EdgeTestSwarm { mut swarm, .. } =
        EdgeSwarmBuilder::new(Membership::new_without_local(from_ref(&Node {
            address: empty_multiaddr,
            id: random_peer_id,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        })))
        .build();
    let message = TestEncapsulatedMessage::new(b"test-payload");
    swarm.send_message(&message);

    // Wait for the first error (no backoff on the initial dial).
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    // Measure the delay until the second error. With exponential backoff, the
    // retry (attempt 2) is delayed by 2^1 = 2 seconds.
    let before_second_error = time::Instant::now();
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;
    let first_backoff = before_second_error.elapsed();
    assert!(first_backoff >= time::Duration::from_secs(2));

    // Measure the delay until the third error. The retry (attempt 3) should be
    // delayed by 2^2 = 4 seconds.
    let before_third_error = time::Instant::now();
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;
    let second_backoff = before_third_error.elapsed();
    assert!(second_backoff >= time::Duration::from_secs(4));
}
