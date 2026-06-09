use core::slice::from_ref;

use lb_blend::{
    message::crypto::key_ext::Ed25519SecretKeyExt as _,
    scheduling::membership::{Membership, Node},
};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use lb_libp2p::{Protocol, SwarmEvent};
use libp2p::{Multiaddr, PeerId};
use test_log::test;
use tokio::time;

use crate::{
    edge::backends::libp2p::tests::utils::{
        SwarmBuilder as EdgeSwarmBuilder, TestSwarm as EdgeTestSwarm,
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
