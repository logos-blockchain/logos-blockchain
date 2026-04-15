use core::slice::from_ref;

use lb_blend::{
    message::crypto::key_ext::Ed25519SecretKeyExt as _,
    scheduling::membership::{Membership, Node},
};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use lb_libp2p::{Protocol, SwarmEvent};
use libp2p::{Multiaddr, PeerId};
use test_log::test;
use tokio::spawn;

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
    // Pause time so that exponential backoff delays are auto-advanced.
    tokio::time::pause();

    let random_peer_id = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();

    // Configure swarm with an unreachable member.
    let EdgeTestSwarm { mut swarm, .. } =
        EdgeSwarmBuilder::new(Membership::new_without_local(from_ref(&Node {
            address: empty_multiaddr.clone(),
            id: random_peer_id,
            public_key: UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
        })))
        .build();
    let message = TestEncapsulatedMessage::new(b"test-payload");
    swarm.send_message(&message);

    let dial_attempt_1_record = swarm
        .pending_dials()
        .filter(|((peer_id, _), _)| peer_id == &random_peer_id)
        .map(|(_, value)| value)
        .next()
        .unwrap();
    assert_eq!(dial_attempt_1_record.address, empty_multiaddr);
    assert_eq!(dial_attempt_1_record.attempt_number, 1.try_into().unwrap());
    assert_eq!(dial_attempt_1_record.message, message.clone());

    // We poll the swarm until we know the first dial attempt has failed.
    // After failure, a retry is scheduled with exponential backoff.
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;
    assert_eq!(swarm.pending_dials().retry_count(), 1);

    // We poll the swarm until the next failure. The backoff timer auto-advances,
    // the retry fires, and then fails again.
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;
    assert_eq!(swarm.pending_dials().retry_count(), 1);

    // We poll the swarm until the next failure, after which there should be no more
    // attempts.
    swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    // Storage should be cleared up, and since there is no other peer, there is
    // no new peer that is dialed.
    assert!(swarm.pending_dials().active().is_empty());
    assert_eq!(swarm.pending_dials().retry_count(), 0);
}

#[test(tokio::test)]
async fn edge_redial_different_peer_after_redial_limit() {
    // Pause time so that exponential backoff delays are auto-advanced.
    tokio::time::pause();

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
    let EdgeTestSwarm {
        swarm: mut edge_swarm,
        ..
    } = EdgeSwarmBuilder::new(edge_membership).build();

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
