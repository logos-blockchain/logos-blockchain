use core::time::Duration;

use lb_blend::{
    network::core::with_core::behaviour::NegotiatedPeerState, scheduling::membership::Node,
};
use libp2p::core::Endpoint;
use test_log::test;
use tokio::{
    select,
    time::{sleep, timeout},
};

use crate::{
    core::backends::{
        BackendEpochInfo,
        libp2p::{
            core_swarm_test_utils::{new_nodes_with_empty_address, update_nodes},
            swarm::BlendSwarmMessage,
            tests::utils::{
                BlendBehaviourBuilder, InnerSwarm, SwarmBuilder, SwarmExt as _, TestProofsVerifier,
                TestSwarm, build_membership,
            },
        },
    },
    test_utils::TestEncapsulatedMessage,
};

#[ignore = "TODO: enable this logic after investigating epoch transition issues. Test disabled because we don't let connections turn unhealthy because of too little messages now until we have proper observation window values."]
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

#[expect(clippy::too_many_lines, reason = "Test function.")]
#[ignore = "TODO: enable this logic after investigating epoch transition issues. Test disabled because we don't let connections turn spammy because of too many messages now until we have proper observation window values."]
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
        .force_send_message_to_current_epoch_peer(&message_1, listening_swarm_peer_id)
        .unwrap();
    malicious_swarm
        .behaviour_mut()
        .blend
        .with_core_mut()
        .force_send_message_to_current_epoch_peer(&message_2, listening_swarm_peer_id)
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
            .blend
            .with_core()
            .is_blocked(malicious_swarm.local_peer_id())
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

/// Polls both swarms until `condition` holds on the first one or `duration`
/// elapses, returning whether it held.
async fn poll_both_until(
    first: &mut InnerSwarm,
    second: &mut InnerSwarm,
    duration: Duration,
    condition: impl Fn(&InnerSwarm) -> bool + Send + Sync,
) -> bool {
    timeout(duration, async {
        // Checked before every poll rather than after the first swarm's
        // events only: the condition may already hold, or become true while
        // the other swarm is the one making progress.
        while !condition(first) {
            select! {
                () = first.poll_next() => {}
                () = second.poll_next() => {}
            }
        }
    })
    .await
    .is_ok()
}

/// Two core nodes. The listening one rejects every `PoQ`, which is what a
/// node does to the messages of a peer whose proofs were generated for a
/// different epoch than the one the connection was accepted under. The dialing
/// one connects and sends a single message, and ends up blocked.
async fn block_dialing_peer_for_invalid_poq() -> (
    InnerSwarm,
    InnerSwarm,
    tokio::sync::mpsc::Sender<BlendSwarmMessage<TestProofsVerifier>>,
    Vec<Node<libp2p::PeerId>>,
) {
    let (mut identities, mut nodes) = new_nodes_with_empty_address(2);

    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (dialing_node, _) = dialing_swarm.listen_and_return_membership_entry(None).await;
    update_nodes(&mut nodes, &dialing_node.id, dialing_node.address.clone());

    let TestSwarm {
        swarm: mut listening_swarm,
        swarm_message_sender,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes).build(|id, membership| {
        BlendBehaviourBuilder::new(id, membership)
            .with_rejecting_proofs_verifier()
            .build()
    });
    let (listening_node, _) = listening_swarm
        .listen_and_return_membership_entry(None)
        .await;
    update_nodes(
        &mut nodes,
        &listening_node.id,
        listening_node.address.clone(),
    );

    dialing_swarm.dial_peer_at_addr(listening_node.id, listening_node.address.clone());
    assert!(
        poll_both_until(
            &mut listening_swarm,
            &mut dialing_swarm,
            Duration::from_secs(5),
            |swarm| {
                swarm
                    .behaviour()
                    .blend
                    .with_core()
                    .negotiated_peers()
                    .contains_key(&dialing_node.id)
            }
        )
        .await,
        "connection was not negotiated by the listening swarm"
    );
    // The handler events are asynchronous, so the dialing side may learn of
    // the negotiation a little later than the listening side.
    assert!(
        poll_both_until(
            &mut dialing_swarm,
            &mut listening_swarm,
            Duration::from_secs(5),
            |swarm| {
                swarm
                    .behaviour()
                    .blend
                    .with_core()
                    .negotiated_peers()
                    .contains_key(&listening_node.id)
            }
        )
        .await,
        "connection was not negotiated by the dialing swarm"
    );

    dialing_swarm
        .behaviour_mut()
        .blend
        .with_core_mut()
        .force_send_message_to_current_epoch_peer(
            &TestEncapsulatedMessage::new(b"proof-for-another-epoch"),
            listening_node.id,
        )
        .unwrap();

    // The block is issued with the verdict, before the connection is torn
    // down, so wait for both.
    assert!(
        poll_both_until(
            &mut listening_swarm,
            &mut dialing_swarm,
            Duration::from_secs(10),
            |swarm| {
                let core_behaviour = swarm.behaviour().blend.with_core();
                core_behaviour.is_blocked(&dialing_node.id)
                    && !core_behaviour
                        .negotiated_peers()
                        .contains_key(&dialing_node.id)
            }
        )
        .await,
        "a peer whose PoQ fails verification must be blocked and disconnected"
    );

    (listening_swarm, dialing_swarm, swarm_message_sender, nodes)
}

#[test(tokio::test)]
async fn peer_with_invalid_poq_is_blocked() {
    let (listening_swarm, dialing_swarm, _sender, _nodes) =
        block_dialing_peer_for_invalid_poq().await;
    assert!(
        listening_swarm
            .behaviour()
            .blend
            .with_core()
            .is_blocked(dialing_swarm.local_peer_id())
    );
    // The spammy connection was closed, and nothing else is negotiated.
    assert_eq!(
        listening_swarm
            .behaviour()
            .blend
            .with_core()
            .num_negotiated_peers(),
        0
    );
}

/// A block must not outlive the epoch it was issued in: the next epoch
/// rebuilds the membership and brings new `PoQ` public inputs, under which the
/// rejected peer's messages may well verify. Here the new epoch's verifier
/// accepts everything and the blocked peer is the only other member, so the
/// listening swarm must unblock it and negotiate with it again.
#[test(tokio::test)]
async fn blocked_peer_is_dialable_again_in_next_epoch() {
    let (mut listening_swarm, mut dialing_swarm, swarm_message_sender, nodes) =
        block_dialing_peer_for_invalid_poq().await;
    let dialing_peer_id = *dialing_swarm.local_peer_id();
    let listening_peer_id = *listening_swarm.local_peer_id();

    swarm_message_sender
        .send(BlendSwarmMessage::StartNewEpoch(BackendEpochInfo {
            membership: build_membership(&nodes, Some(listening_peer_id)),
            epoch: 2.into(),
            proofs_verifier: TestProofsVerifier::default(),
        }))
        .await
        .unwrap();

    let renegotiated = poll_both_until(
        &mut listening_swarm,
        &mut dialing_swarm,
        Duration::from_secs(5),
        |swarm| {
            swarm
                .behaviour()
                .blend
                .with_core()
                .negotiated_peers()
                .contains_key(&dialing_peer_id)
        },
    )
    .await;

    assert!(
        !listening_swarm
            .behaviour()
            .blend
            .with_core()
            .is_blocked(&dialing_peer_id),
        "a peer blocked in epoch 1 is still blocked in epoch 2"
    );
    assert!(
        renegotiated,
        "the listening swarm did not reconnect to the peer it blocked in the previous epoch"
    );
}
