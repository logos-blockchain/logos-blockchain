use core::time::Duration;
use std::collections::HashSet;

use futures::StreamExt as _;
use lb_libp2p::SwarmEvent;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::{TestEncapsulatedMessage, TestSwarm},
    with_core::{
        behaviour::{
            Event, NegotiatedPeerState, SpamReason,
            message_cache::MessageStatus,
            tests::utils::{
                BehaviourBuilder, SwarmExt as _, build_memberships, new_nodes_with_empty_address,
            },
        },
        error::SendError,
    },
};

#[test(tokio::test)]
async fn message_sending_and_reception() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // Send one message, which is within the range of expected messages.
    let test_message = TestEncapsulatedMessage::new(b"msg");
    let test_message_id = test_message.id();
    dialing_swarm
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &test_message.as_ref().clone().into(),
        )
        .unwrap();

    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, sender, .. }) = listening_event {
                    assert_eq!(sender, *dialing_swarm.local_peer_id());
                    assert_eq!(*message, test_message.clone().into_inner().into());
                    break;
                }
            }
        }
    }

    assert_eq!(
        dialing_swarm
            .behaviour()
            .message_cache
            .message_status(&test_message_id)
            .unwrap(),
        &MessageStatus::Forwarded
    );
    assert_eq!(
        listening_swarm
            .behaviour()
            .message_cache
            .message_status(&test_message_id)
            .unwrap(),
        &MessageStatus::Processed
    );
    assert_eq!(
        listening_swarm
            .behaviour()
            .message_cache
            .messages_from_peer(dialing_swarm.local_peer_id())
            .collect::<HashSet<_>>(),
        vec![test_message_id].into_iter().collect::<HashSet<_>>()
    );

    // Second copy of the message should not be sent because it was already
    // processed.
    assert_eq!(
        dialing_swarm
            .behaviour_mut()
            .publish_message_with_validated_signature_to_current_session(
                &test_message.as_ref().clone().into()
            ),
        Err(SendError::DuplicateMessage)
    );
}

#[test(tokio::test)]
async fn undeserializable_message_received() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    dialing_swarm
        .behaviour_mut()
        .force_send_serialized_message_to_peer(b"msg".to_vec(), *listening_swarm.local_peer_id())
        .unwrap();

    let mut events_to_match = 2u8;
    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, peer_state)) => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert_eq!(peer_state, NegotiatedPeerState::Spammy(SpamReason::UndeserializableMessage));
                        events_to_match -= 1;
                    }
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                        events_to_match -= 1;
                    }
                    _ => {}
                }
            }
        }
        if events_to_match == 0 {
            break;
        }
    }
}

#[test(tokio::test)]
async fn duplicate_message_received_from_same_peer() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    let test_message = TestEncapsulatedMessage::new(b"msg");
    dialing_swarm
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &test_message.as_ref().clone().into(),
        )
        .unwrap();

    // Poll both swarms until the first message is fully received by the listener.
    // Without this, the message stays queued in the behaviour and is never sent
    // over the wire, causing both messages to arrive in the same connection
    // monitor window and triggering `TooManyMessages` instead of
    // `DuplicateMessage`.
    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { .. }) = listening_event {
                    break;
                }
            }
        }
    }

    // Wait enough time to not considered spammy by the listener.
    sleep(Duration::from_secs(3)).await;

    // This is a duplicate message, so the listener will mark the dialer as spammy.
    dialing_swarm
        .behaviour_mut()
        .force_send_message_to_peer(&test_message.into_inner(), *listening_swarm.local_peer_id())
        .unwrap();

    let mut events_to_match = 2u8;
    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, peer_state)) => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert_eq!(peer_state, NegotiatedPeerState::Spammy(SpamReason::DuplicateMessage));
                        events_to_match -= 1;
                    }
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                        events_to_match -= 1;
                    }
                    _ => {}
                }
            }
        }
        if events_to_match == 0 {
            break;
        }
    }
}

#[test(tokio::test)]
async fn duplicate_message_received_from_different_peers() {
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let mut dialing_swarm_1 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut dialing_swarm_2 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(1..=2)
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm_1
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;
    dialing_swarm_2
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    let test_message = TestEncapsulatedMessage::new(b"msg");
    dialing_swarm_1
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &test_message.as_ref().clone().into(),
        )
        .unwrap();
    dialing_swarm_2
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &test_message.as_ref().clone().into(),
        )
        .unwrap();

    // Verify that the message is bubbled up to the swarm only once
    let mut received_message_count = 0u8;
    loop {
        select! {
            () = sleep(Duration::from_secs(5)) => {
                break;
            }
            _ = dialing_swarm_1.select_next_some() => {}
            _ = dialing_swarm_2.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { .. }) = listening_event {
                    received_message_count += 1;
                }
            }
        }
    }
    assert_eq!(received_message_count, 1);
}

#[test(tokio::test)]
async fn invalid_signature_message_received() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    let invalid_public_header_message = TestEncapsulatedMessage::new_with_invalid_signature(b"");
    dialing_swarm
        .behaviour_mut()
        .force_send_message_to_peer(
            &invalid_public_header_message.as_ref().clone(),
            *listening_swarm.local_peer_id(),
        )
        .unwrap();

    let mut events_to_match = 2u8;
    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, peer_state)) => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert_eq!(peer_state, NegotiatedPeerState::Spammy(SpamReason::InvalidHeaderSignature));
                        events_to_match -= 1;
                    }
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert!(endpoint.is_listener());

                        events_to_match -= 1;
                    }
                    _ => {}
                }
            }
        }
        if events_to_match == 0 {
            break;
        }
    }
}

#[test(tokio::test)]
async fn message_already_forwarded_silently_ignored_when_received_from_peer() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut node_a = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut node_b = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    node_b.listen().with_memory_addr_external().await;
    node_a.connect_and_wait_for_upgrade(&mut node_b).await;

    let test_message = TestEncapsulatedMessage::new(b"msg");

    // Node A forwards X to Node B. In Node A's cache X is now `Forwarded`.
    node_a
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &test_message.as_ref().clone().into(),
        )
        .unwrap();

    // Wait until Node B has received the message.
    loop {
        select! {
            _ = node_a.select_next_some() => {}
            event = node_b.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { .. }) = event {
                    break;
                }
            }
        }
    }

    // Node B sends X back to Node A (bypassing Node B's own Forwarded check).
    // From Node A's perspective X is already `Forwarded`, so the
    // `is_message_processed` guard should fire and the message must be
    // silently dropped - no event, no spam marking.
    node_b
        .behaviour_mut()
        .force_send_message_to_peer(&test_message.into_inner(), *node_a.local_peer_id())
        .unwrap();

    let mut node_a_got_message = false;
    let mut node_a_got_disconnect = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(3)) => { break; }
            event = node_a.select_next_some() => {
                match event {
                    SwarmEvent::Behaviour(Event::Message { .. }) => {
                        node_a_got_message = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(..)) => {
                        node_a_got_disconnect = true;
                    }
                    _ => {}
                }
            }
            _ = node_b.select_next_some() => {}
        }
    }

    assert!(
        !node_a_got_message,
        "Node A must not emit a Message event for a message it already forwarded"
    );
    assert!(
        !node_a_got_disconnect,
        "Node A must not mark Node B as spammy for sending an already-forwarded message"
    );
}

#[test(tokio::test)]
async fn duplicate_message_in_old_session_does_not_disconnect_peer() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut sender = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut receiver = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    receiver.listen().with_memory_addr_external().await;
    sender.connect_and_wait_for_upgrade(&mut receiver).await;

    let test_message = TestEncapsulatedMessage::new(b"msg");

    // Sender publishes X. Receiver marks it as `Processed` in its cache.
    sender
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &test_message.as_ref().clone().into(),
        )
        .unwrap();

    loop {
        select! {
            _ = sender.select_next_some() => {}
            event = receiver.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { .. }) = event {
                    break;
                }
            }
        }
    }

    // Receiver starts a new session. Sender's connection moves to the old
    // session together with the existing message cache (which contains X as
    // `Processed`).
    let memberships = build_memberships(&[&sender, &receiver]);
    receiver
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), 1));

    // Wait long enough so that the connection monitor does not fire
    // `TooManyMessages` instead.
    sleep(Duration::from_secs(3)).await;

    // Sender sends X again, bypassing its own `Forwarded` guard. From
    // receiver's point of view this arrives over the old-session connection.
    // The old-session handler detects a duplicate from the same peer and
    // returns `Err(DuplicateMessageFromPeer)`, but it currently must NOT close the
    // connection as spammy.
    sender
        .behaviour_mut()
        .force_send_message_to_peer(&test_message.into_inner(), *receiver.local_peer_id())
        .unwrap();

    let mut peer_disconnected = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(3)) => { break; }
            _ = sender.select_next_some() => {}
            event = receiver.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::PeerDisconnected(..)) = event {
                    peer_disconnected = true;
                }
            }
        }
    }

    assert!(
        !peer_disconnected,
        "Receiver must not disconnect sender for a duplicate message received over the old-session connection"
    );
}

#[test(tokio::test)]
async fn duplicate_message_from_old_session_after_session_rotation_is_suppressed() {
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let mut sender_a = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut sender_b = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut receiver = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(1..=2)
            .build()
    });

    receiver.listen().with_memory_addr_external().await;
    sender_a.connect_and_wait_for_upgrade(&mut receiver).await;
    sender_b.connect_and_wait_for_upgrade(&mut receiver).await;

    // Sender A sends message X. Receiver processes it and stores X as
    // `Processed` in its current-session message cache.
    let test_message = TestEncapsulatedMessage::new(b"msg");
    sender_a
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &test_message.as_ref().clone().into(),
        )
        .unwrap();

    loop {
        select! {
            _ = sender_a.select_next_some() => {}
            _ = sender_b.select_next_some() => {}
            receiver_event = receiver.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { .. }) = receiver_event {
                    break;
                }
            }
        }
    }

    // Receiver starts a new session. The message cache now containing X
    // as `Processed` is transferred into the old session object,
    // alongside the connections to both sender_a and sender_b.
    let memberships = build_memberships(&[&sender_a, &sender_b, &receiver]);
    receiver
        .behaviour_mut()
        .start_new_session((memberships[2].clone(), 1));

    // Sender B sends the identical message X through its (still-open)
    // connection to receiver. From receiver's point of view this connection
    // now belongs to the old session. Because X is already in the transferred
    // cache, receiver must NOT emit a second `Message` event.
    sender_b
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &test_message.as_ref().clone().into(),
        )
        .unwrap();

    let mut duplicate_message_received = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(5)) => { break; }
            _ = sender_a.select_next_some() => {}
            _ = sender_b.select_next_some() => {}
            receiver_event = receiver.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { .. }) = receiver_event {
                    duplicate_message_received = true;
                }
            }
        }
    }

    assert!(
        !duplicate_message_received,
        "Receiver must not re-emit a message that was already processed in the previous session"
    );
}
