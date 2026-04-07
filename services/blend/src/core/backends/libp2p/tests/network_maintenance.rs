use core::time::Duration;

use lb_blend::{
    network::core::with_core::behaviour::NegotiatedPeerState, scheduling::membership::Node,
};
use libp2p::core::Endpoint;
use test_log::test;
use tokio::{select, time::sleep};

use crate::{
    core::backends::libp2p::{
        core_swarm_test_utils::{new_nodes_with_empty_address, update_nodes},
        tests::utils::{BlendBehaviourBuilder, SwarmBuilder, SwarmExt as _, TestSwarm},
    },
    test_utils::TestEncapsulatedMessage,
};

#[ignore = "TODO: enable this logic after investigating session/epoch transition issues. Test disabled because we don't let connections turn unhealthy because of too little messages now until we have proper observation window values."]
#[test(tokio::test)]
async fn on_unhealthy_peer() {
    let (mut identities, mut nodes) = new_nodes_with_empty_address(3);
    let TestSwarm {
        swarm: mut unhealthy_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let TestSwarm {
        swarm: mut second_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (second_node, _) = second_swarm.listen_and_return_membership_entry(None).await;
    update_nodes(&mut nodes, &second_node.id, second_node.address);

    let TestSwarm {
        swarm: mut listening_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes).build(|id, membership| {
        BlendBehaviourBuilder::new(id, membership)
            // Listening swarm expects at least one message per observation window to keep
            // connection healthy.
            .with_observation_window(Duration::from_secs(2), 1..=2)
            .build()
    });
    let (
        Node {
            address: listening_swarm_address,
            id: listening_swarm_peer_id,
            ..
        },
        _,
    ) = listening_swarm
        .listen_and_return_membership_entry(None)
        .await;

    // The unhealthy swarm dials the listening swarm, and then does not send any
    // messages, prompting the listening swarm to open a new connection with another
    // swarm.
    unhealthy_swarm.dial_peer_at_addr(listening_swarm_peer_id, listening_swarm_address);

    loop {
        select! {
            // Wait for the connection to be established + a timeout for the connection monitor, which will trigger a new connection with the other swarm.
            () = sleep(Duration::from_secs(3)) => {
                break;
            }
            () = listening_swarm.poll_next() => {}
            () = unhealthy_swarm.poll_next() => {}
            () = second_swarm.poll_next() => {}
        }
    }

    let unhealthy_swarm_connection_details = listening_swarm
        .behaviour()
        .blend
        .with_core()
        .negotiated_peers()
        .get(unhealthy_swarm.local_peer_id())
        .unwrap();
    assert_eq!(
        unhealthy_swarm_connection_details.negotiated_state(),
        NegotiatedPeerState::Unhealthy
    );

    let second_swarm_connection_details = listening_swarm
        .behaviour()
        .blend
        .with_core()
        .negotiated_peers()
        .get(second_swarm.local_peer_id())
        .unwrap();
    assert_eq!(second_swarm_connection_details.role(), Endpoint::Listener);
}

#[ignore = "TODO: enable this logic after investigating session/epoch transition issues. Test disabled because we don't let connections turn spammy because of too many messages now until we have proper observation window values."]
#[test(tokio::test)]
async fn on_malicious_peer() {
    let (mut identities, mut nodes) = new_nodes_with_empty_address(3);

    let TestSwarm {
        swarm: mut second_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (second_node, _) = second_swarm.listen_and_return_membership_entry(None).await;
    update_nodes(&mut nodes, &second_node.id, second_node.address);

    let TestSwarm {
        swarm: mut listening_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes).build(|id, membership| {
        BlendBehaviourBuilder::new(id, membership)
            // Listening swarm expects at most one message per observation window to keep
            // connection healthy.
            .with_observation_window(Duration::from_secs(2), 0..=1)
            .build()
    });
    let (
        Node {
            address: listening_swarm_address,
            id: listening_swarm_peer_id,
            ..
        },
        _,
    ) = listening_swarm
        .listen_and_return_membership_entry(None)
        .await;
    update_nodes(
        &mut nodes,
        &listening_swarm_peer_id,
        listening_swarm_address.clone(),
    );

    let TestSwarm {
        swarm: mut malicious_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes).build(|id, membership| {
        BlendBehaviourBuilder::new(id, membership)
            // We use `0` as the minimum message frequency so we know that the listening peer won't
            // be marked as unhealthy by this swarm.
            .with_observation_window(Duration::from_secs(10), 0..=2)
            .build()
    });

    // The unhealthy swarm dials the listening swarm, and then sends more than the
    // maximum number of expected messages, prompting the listening swarm to close
    // this connection and mark the peer as spammy.
    malicious_swarm.dial_peer_at_addr(listening_swarm_peer_id, listening_swarm_address);

    loop {
        select! {
            // Wait for the connection to be established.
            () = sleep(Duration::from_secs(1)) => {
                break;
            }
            () = listening_swarm.poll_next() => {}
            () = malicious_swarm.poll_next() => {}
            () = second_swarm.poll_next() => {}
        }
    }

    // The malicious swarm sends two messages to the listening swarm, which expects
    // at most one message per observation window.
    let message_1 = TestEncapsulatedMessage::new(b"test-payload-1");
    let message_2 = TestEncapsulatedMessage::new(b"test-payload-2");
    malicious_swarm
        .behaviour_mut()
        .blend
        .with_core_mut()
        .force_send_message_to_peer(&message_1, listening_swarm_peer_id)
        .unwrap();
    malicious_swarm
        .behaviour_mut()
        .blend
        .with_core_mut()
        .force_send_message_to_peer(&message_2, listening_swarm_peer_id)
        .unwrap();

    loop {
        select! {
            // Wait for the messages to be delivered and for a new connection to be established.
            () = sleep(Duration::from_secs(4)) => {
                break;
            }
            () = listening_swarm.poll_next() => {}
            () = malicious_swarm.poll_next() => {}
            () = second_swarm.poll_next() => {}
        }
    }

    // We check that the malicious peer has been blacklisted.
    assert!(
        listening_swarm
            .behaviour()
            .blocked_peers
            .blocked_peers()
            .contains(malicious_swarm.local_peer_id())
    );

    // We check that the malicious peer has no entry in the set of negotiated peers.
    assert!(
        !listening_swarm
            .behaviour()
            .blend
            .with_core()
            .negotiated_peers()
            .contains_key(malicious_swarm.local_peer_id())
    );

    // We check that the other swarm has a negotiated connection with the listening
    // swarm.
    let second_swarm_connection_details = listening_swarm
        .behaviour()
        .blend
        .with_core()
        .negotiated_peers()
        .get(second_swarm.local_peer_id())
        .unwrap();
    assert_eq!(
        second_swarm_connection_details.negotiated_state(),
        NegotiatedPeerState::Healthy
    );
    assert_eq!(second_swarm_connection_details.role(), Endpoint::Listener);
}
