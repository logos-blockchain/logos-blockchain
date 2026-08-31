use core::slice::from_ref;

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
    edge::backends::libp2p::{
        swarm::Command,
        tests::utils::{SwarmBuilder as EdgeSwarmBuilder, TestSwarm as EdgeTestSwarm},
    },
    test_utils::TestEncapsulatedMessage,
};

/// Verifies that a message whose chosen peer is unreachable is retried with
/// exponential backoff and then dropped once all attempts are exhausted.
#[test(tokio::test)]
async fn edge_drops_message_after_exhausting_attempts() {
    let random_peer_id = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();

    // Configure swarm with a single unreachable member.
    let EdgeTestSwarm { mut swarm, .. } =
        EdgeSwarmBuilder::new(Membership::new_without_local(from_ref(&Node {
            address: empty_multiaddr.clone(),
            id: random_peer_id,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        })))
        .with_max_dial_attempts(3)
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
    // The single chosen peer is retried with exponential backoff; we never fall
    // back to a different peer.
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

    // All attempts exhausted: the message is dropped and nothing remains
    // pending. We do not pick a new peer to retry with.
    assert!(
        swarm.pending_dials().is_empty(),
        "Message should be dropped after exhausting all attempts for the chosen peer"
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

/// A send that never completes (e.g. a peer that accepted the connection but
/// never finished stream negotiation) must not wedge the event loop: work
/// queued behind it should still be processed.
///
/// This guards the fix that moved the stream-open/send chain off the `select!`
/// loop into the `pending_events` queue. Previously the chain was awaited
/// inline, so a single unresponsive peer stalled command and retry handling
/// until restart.
#[test(tokio::test)]
async fn stalled_send_does_not_block_command_processing() {
    let peer_id = PeerId::random();
    let address: Multiaddr = Protocol::Memory(0).into();

    let EdgeTestSwarm {
        mut swarm,
        command_sender,
    } = EdgeSwarmBuilder::new(Membership::new_without_local(from_ref(&Node {
        address,
        id: peer_id,
        public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
    })))
    .build();

    // Simulate an in-flight send that will never resolve, occupying the
    // pending-events queue indefinitely.
    swarm.push_stalled_send();

    // A send command arrives while that send is stuck.
    let message = TestEncapsulatedMessage::new(b"test-payload");
    command_sender
        .send(Command::SendMessage(message.clone()))
        .await
        .unwrap();

    // A single loop iteration must pick up the command (and schedule a dial)
    // rather than blocking behind the stalled send.
    swarm.poll_next().await;

    assert_eq!(
        swarm.pending_dials().len(),
        1,
        "The command should be processed even though a send is stalled"
    );
}

/// A dial that fails because the host at the declared address presents a
/// different identity than the membership expects (`DialError::WrongPeerId`)
/// must not be retried: no number of attempts changes the remote's key.
///
/// This is the failure that took down Blend on testnet v0.2.1 — every declared
/// locator pointed at a host holding a different key, and because the error was
/// funnelled into the same backoff ladder as a timeout it was reported as
/// "peer was not reachable after N attempts" rather than as a configuration
/// fault.
#[test(tokio::test)]
async fn edge_does_not_retry_unrecoverable_dial_failure() {
    // A real listening swarm, whose address we will pair with the wrong peer id.
    let (mut identities, nodes) = new_nodes_with_empty_address(1);
    let CoreTestSwarm {
        swarm: mut listening_swarm,
        ..
    } = CoreSwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (listening_node, _) = listening_swarm
        .listen_and_return_membership_entry(None)
        .await;
    spawn(async move { listening_swarm.run().await });

    // The membership points at the right address but the wrong identity.
    let wrong_peer_id = PeerId::random();
    let EdgeTestSwarm { mut swarm, .. } =
        EdgeSwarmBuilder::new(Membership::new_without_local(from_ref(&Node {
            address: listening_node.address.clone(),
            id: wrong_peer_id,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        })))
        // Generous retry budget: if the error were treated as recoverable we
        // would see further attempts after the backoff below.
        .with_max_dial_attempts(3)
        .build();

    swarm.send_message(&TestEncapsulatedMessage::new(b"test-payload"));

    // The first (and only) dial fails with a wrong-peer-id error.
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(wrong_peer_id)
        })
        .await;

    // The first backoff would be 2^1 = 2s. Wait past it: no second attempt may
    // be made, and the peer must be gone from the pending dials.
    let no_second_attempt = time::timeout(
        time::Duration::from_secs(4),
        swarm.poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(wrong_peer_id)
        }),
    )
    .await;
    assert!(
        no_second_attempt.is_err(),
        "an unrecoverable dial failure must not be retried"
    );
    assert!(
        swarm.pending_dials().is_empty(),
        "the abandoned peer must not be left pending"
    );
}

/// A peer abandoned as unrecoverable stays excluded for the rest of the
/// membership's life, so later messages are not spent re-dialing it.
#[test(tokio::test)]
async fn edge_excludes_unrecoverable_peer_from_later_sends() {
    let (mut identities, nodes) = new_nodes_with_empty_address(1);
    let CoreTestSwarm {
        swarm: mut listening_swarm,
        ..
    } = CoreSwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (listening_node, _) = listening_swarm
        .listen_and_return_membership_entry(None)
        .await;
    spawn(async move { listening_swarm.run().await });

    let wrong_peer_id = PeerId::random();
    let EdgeTestSwarm { mut swarm, .. } =
        EdgeSwarmBuilder::new(Membership::new_without_local(from_ref(&Node {
            address: listening_node.address.clone(),
            id: wrong_peer_id,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        })))
        .build();

    swarm.send_message(&TestEncapsulatedMessage::new(b"first"));
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(wrong_peer_id)
        })
        .await;

    // The only member is now known-unusable, so a second message finds no peer
    // to dial at all rather than repeating the doomed dial.
    swarm.send_message(&TestEncapsulatedMessage::new(b"second"));
    assert!(
        swarm.pending_dials().is_empty(),
        "a peer abandoned as unrecoverable must not be chosen for later messages"
    );
}
