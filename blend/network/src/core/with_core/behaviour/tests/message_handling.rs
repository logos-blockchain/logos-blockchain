use core::time::Duration;

use futures::StreamExt as _;
use lb_blend_message::serialize_encapsulated_message_with_verified_public_header;
use lb_libp2p::SwarmEvent;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{
    select,
    time::{sleep, timeout},
};

use crate::core::{
    tests::utils::{
        TestEncapsulatedMessage, TestProofsVerifier, TestSwarm, undecodable_message_bytes,
    },
    with_core::{
        behaviour::{
            Event,
            blacklist::BlacklistReason,
            message_cache::MessageStatus,
            tests::utils::{
                BehaviourBuilder, PEERING_DEGREE, SwarmExt as _, build_memberships,
                new_nodes_with_empty_address,
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
        .publish_message_with_validated_header_to_current_epoch(test_message.as_ref())
        .unwrap();

    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, sender, .. }) = listening_event {
                    assert_eq!(sender, *dialing_swarm.local_peer_id());
                    assert_eq!(*message, test_message.clone().into_inner());
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
    // Second copy of the message should not be sent because it was already
    // processed.
    assert_eq!(
        dialing_swarm
            .behaviour_mut()
            .publish_message_with_validated_header_to_current_epoch(test_message.as_ref()),
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
        .force_send_serialized_message_to_current_epoch_peer(
            &undecodable_message_bytes(),
            *listening_swarm.local_peer_id(),
        )
        .unwrap();

    let mut events_to_match = 3u8;
    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id)) => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        events_to_match -= 1;
                    }
                    SwarmEvent::Behaviour(Event::PeerBlacklisted { peer, reason }) => {
                        assert_eq!(peer, *dialing_swarm.local_peer_id());
                        assert_eq!(reason, BlacklistReason::UndeserializableMessage);
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
async fn message_with_unexpected_layer_count_disconnects_peer() {
    // The listening node expects a single encapsulation layer, but the sender
    // delivers a well-formed 3-layer message. The size gate in
    // `EncapsulatedMessage::deserialize_from_remote` rejects it up front, and
    // the sender is treated exactly like one delivering undeserializable bytes.
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_num_blend_layers(1)
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    let message = TestEncapsulatedMessage::new(b"unexpected_layer_count");
    dialing_swarm
        .behaviour_mut()
        .force_send_serialized_message_to_current_epoch_peer(
            &serialize_encapsulated_message_with_verified_public_header(message.as_ref()),
            *listening_swarm.local_peer_id(),
        )
        .unwrap();

    let mut events_to_match = 3u8;
    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id)) => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        events_to_match -= 1;
                    }
                    // The reason is deliberately not pinned. With no length on
                    // the wire, a node reads the number of bytes its own layer
                    // count implies, so a message built for a different one is
                    // read as a truncated prefix and fails whichever check the
                    // misread bytes reach first. What matters is that it is
                    // never accepted and the sender is excluded.
                    SwarmEvent::Behaviour(Event::PeerBlacklisted { peer, .. }) => {
                        assert_eq!(peer, *dialing_swarm.local_peer_id());
                        events_to_match -= 1;
                    }
                    SwarmEvent::Behaviour(Event::Message { .. }) => {
                        panic!("A message built for a different layer count must never be accepted.");
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
async fn a_duplicate_from_the_same_peer_carries_no_reaction() {
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
        .publish_message_with_validated_header_to_current_epoch(test_message.as_ref())
        .unwrap();

    // Poll both swarms until the first copy is fully received by the listener,
    // so that the second one really is a duplicate rather than the first
    // message still sitting in a queue.
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

    // The same message again. An honest node relaying along two paths produces
    // duplicates as a matter of course, so the listener must neither report it
    // twice nor hold it against the sender.
    dialing_swarm
        .behaviour_mut()
        .force_send_message_to_current_epoch_peer(
            &test_message.into_inner(),
            *listening_swarm.local_peer_id(),
        )
        .unwrap();

    let reaction = timeout(Duration::from_secs(2), async {
        loop {
            select! {
                _ = dialing_swarm.select_next_some() => {}
                listening_swarm_event = listening_swarm.select_next_some() => {
                    match listening_swarm_event {
                        SwarmEvent::Behaviour(Event::Message { .. }) => {
                            return "the duplicate was reported to the swarm a second time";
                        }
                        SwarmEvent::Behaviour(Event::PeerBlacklisted { .. }) => {
                            return "the sender was blacklisted";
                        }
                        SwarmEvent::Behaviour(Event::PeerDisconnected(_))
                        | SwarmEvent::ConnectionClosed { .. } => {
                            return "the connection was closed";
                        }
                        _ => {}
                    }
                }
            }
        }
    })
    .await;

    if let Ok(reaction) = reaction {
        panic!("A duplicate must carry no reaction, but {reaction}.");
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
            .with_peering_degree(PEERING_DEGREE)
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
        .publish_message_with_validated_header_to_current_epoch(test_message.as_ref())
        .unwrap();
    dialing_swarm_2
        .behaviour_mut()
        .publish_message_with_validated_header_to_current_epoch(test_message.as_ref())
        .unwrap();

    // Both copies are reported to the swarm: nothing enters the message cache
    // until a `PoQ` verifies, so the second copy cannot be recognised as a
    // duplicate before it has been verified in its own right. What must not
    // happen twice is the relay, so each report is forwarded on as the swarm
    // would, and only the first one is expected to go out.
    let mut forward_results = Vec::new();
    loop {
        select! {
            () = sleep(Duration::from_secs(5)) => {
                break;
            }
            _ = dialing_swarm_1.select_next_some() => {}
            _ = dialing_swarm_2.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, sender, epoch }) = listening_event {
                    forward_results.push(
                        listening_swarm
                            .behaviour_mut()
                            .forward_message_with_verified_public_header(&message, sender, epoch),
                    );
                }
            }
        }
    }

    let (relayed, rejected): (Vec<_>, Vec<_>) =
        forward_results.iter().partition(|result| result.is_ok());
    assert_eq!(
        relayed.len(),
        1,
        "The message must be relayed exactly once, no matter how many peers sent it"
    );
    assert!(
        rejected
            .iter()
            .all(|result| **result == Err(SendError::DuplicateMessage)),
        "Every further copy must be rejected as a duplicate, got {rejected:?}"
    );
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
        .force_send_message_to_current_epoch_peer(
            &invalid_public_header_message.as_ref().clone(),
            *listening_swarm.local_peer_id(),
        )
        .unwrap();

    let mut events_to_match = 3u8;
    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id)) => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        events_to_match -= 1;
                    }
                    SwarmEvent::Behaviour(Event::PeerBlacklisted { peer, reason }) => {
                        assert_eq!(peer, *dialing_swarm.local_peer_id());
                        assert_eq!(reason, BlacklistReason::InvalidHeaderSignature);
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

/// A message whose `PoQ` does not verify is never reported to the swarm — so it
/// can never be relayed — and its sender is blacklisted and disconnected,
/// exactly like a peer sending a message with an invalid signature.
#[test(tokio::test)]
async fn invalid_proof_of_quota_message_received() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    // The receiving side rejects every `PoQ` it is handed.
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_rejecting_proofs_verifier()
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // The message is well-formed and correctly signed: only its `PoQ` fails.
    let test_message = TestEncapsulatedMessage::new(b"invalid-poq");
    dialing_swarm
        .behaviour_mut()
        .publish_message_with_validated_header_to_current_epoch(test_message.as_ref())
        .unwrap();

    let mut events_to_match = 3u8;
    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::Behaviour(Event::Message { .. }) => {
                        panic!("A message whose PoQ failed to verify must not be reported to the swarm");
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id)) => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        events_to_match -= 1;
                    }
                    SwarmEvent::Behaviour(Event::PeerBlacklisted { peer, reason }) => {
                        assert_eq!(peer, *dialing_swarm.local_peer_id());
                        assert_eq!(reason, BlacklistReason::InvalidProofOfQuota);
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
        .publish_message_with_validated_header_to_current_epoch(test_message.as_ref())
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
    // silently dropped - no event, no blacklisting.
    node_b
        .behaviour_mut()
        .force_send_message_to_current_epoch_peer(
            &test_message.into_inner(),
            *node_a.local_peer_id(),
        )
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
        "Node A must not mark Node B as malicious for sending an already-forwarded message"
    );
}

#[test(tokio::test)]
async fn a_duplicate_over_an_old_epoch_connection_carries_no_reaction() {
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
        .publish_message_with_validated_header_to_current_epoch(test_message.as_ref())
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

    // Receiver starts a new epoch. Sender's connection moves to the old
    // epoch together with the existing message cache (which contains X as
    // `Processed`).
    let memberships = build_memberships(&[&sender, &receiver]);
    receiver.behaviour_mut().start_new_epoch(
        (memberships[1].clone(), 1.into()),
        TestProofsVerifier::accepting(),
    );

    // Sender sends X again, bypassing its own `Forwarded` guard. From
    // receiver's point of view this arrives over the old-epoch connection.
    // The old epoch reaches the relay checks by its own path, so it gets its
    // own guard against the duplicate penalty coming back on one side only.
    sender
        .behaviour_mut()
        .force_send_message_to_current_epoch_peer(
            &test_message.into_inner(),
            *receiver.local_peer_id(),
        )
        .unwrap();

    let reaction = timeout(Duration::from_secs(3), async {
        loop {
            select! {
                _ = sender.select_next_some() => {}
                event = receiver.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(Event::PeerBlacklisted { .. }) => {
                            return "the sender was blacklisted";
                        }
                        SwarmEvent::Behaviour(Event::PeerDisconnected(..)) => {
                            return "a disconnection was reported to the swarm";
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. }
                            if peer_id == *sender.local_peer_id() =>
                        {
                            return "the old-epoch connection was closed";
                        }
                        _ => {}
                    }
                }
            }
        }
    })
    .await;

    if let Ok(reaction) = reaction {
        panic!("A duplicate must carry no reaction, but {reaction}.");
    }
}

#[test(tokio::test)]
async fn undeserializable_message_in_old_epoch_closes_connection_without_swarm_notification() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut sender = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut receiver = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    receiver.listen().with_memory_addr_external().await;
    sender.connect_and_wait_for_upgrade(&mut receiver).await;

    // Receiver starts a new epoch. Sender's connection moves to the old
    // epoch.
    let memberships = build_memberships(&[&sender, &receiver]);
    receiver.behaviour_mut().start_new_epoch(
        (memberships[1].clone(), 1.into()),
        TestProofsVerifier::accepting(),
    );

    // Sender sends garbage data over the old-epoch connection.
    sender
        .behaviour_mut()
        .force_send_serialized_message_to_current_epoch_peer(
            &undecodable_message_bytes(),
            *receiver.local_peer_id(),
        )
        .unwrap();

    let mut peer_disconnected_event = false;
    let mut connection_closed = false;
    let mut blacklisted_for = None;
    loop {
        select! {
            () = sleep(Duration::from_secs(15)) => { break; }
            _ = sender.select_next_some() => {}
            event = receiver.select_next_some() => {
                match event {
                    SwarmEvent::Behaviour(Event::PeerDisconnected(..)) => {
                        peer_disconnected_event = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerBlacklisted { peer, reason }) => {
                        assert_eq!(peer, *sender.local_peer_id());
                        blacklisted_for = Some(reason);
                    }
                    SwarmEvent::ConnectionClosed { .. } => {
                        connection_closed = true;
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(
        connection_closed,
        "Connection with a misbehaving old-epoch peer must be closed"
    );
    assert!(
        !peer_disconnected_event,
        "No PeerDisconnected event must be emitted for a misbehaving old-epoch peer"
    );
    // The blacklist belongs to the node, not to an epoch, so an offence over
    // an old-epoch connection excludes the peer just the same.
    assert_eq!(
        blacklisted_for,
        Some(BlacklistReason::UndeserializableMessage)
    );
    assert!(
        receiver
            .behaviour()
            .blacklisted_peers()
            .any(|peer| peer == sender.local_peer_id())
    );
}

/// A peer excluded over one connection is excluded as a peer, so it loses the
/// others too — including the one it holds for the current epoch.
#[test(tokio::test)]
async fn a_peer_that_offends_on_an_old_epoch_connection_loses_its_current_epoch_one() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut sender = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut receiver = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    receiver.listen().with_memory_addr_external().await;
    sender.connect_and_wait_for_upgrade(&mut receiver).await;

    // Receiver starts a new epoch. Sender's connection moves to the old
    // epoch.
    let memberships = build_memberships(&[&sender, &receiver]);
    receiver.behaviour_mut().start_new_epoch(
        (memberships[1].clone(), 1.into()),
        TestProofsVerifier::accepting(),
    );

    // Re-connect for the new epoch.
    sender.behaviour_mut().start_new_epoch(
        (memberships[0].clone(), 1.into()),
        TestProofsVerifier::accepting(),
    );
    sender.connect_and_wait_for_upgrade(&mut receiver).await;
    assert!(
        receiver
            .behaviour()
            .negotiated_peers()
            .contains_key(sender.local_peer_id())
    );

    // Sender sends garbage over the old-epoch connection.
    sender
        .behaviour_mut()
        .force_send_serialized_message_to_peer_at_epoch(
            &undecodable_message_bytes(),
            *receiver.local_peer_id(),
            0.into(),
        )
        .unwrap();

    // The current-epoch connection goes with it, and the swarm is told, so
    // that it can dial a replacement.
    let disconnected = timeout(Duration::from_secs(15), async {
        loop {
            select! {
                _ = sender.select_next_some() => {}
                event = receiver.select_next_some() => {
                    if let SwarmEvent::Behaviour(Event::PeerDisconnected(peer)) = event
                        && peer == *sender.local_peer_id()
                    {
                        return;
                    }
                }
            }
        }
    })
    .await;
    assert!(
        disconnected.is_ok(),
        "the peer kept its current-epoch connection after being blacklisted"
    );
    assert!(
        !receiver
            .behaviour()
            .negotiated_peers()
            .contains_key(sender.local_peer_id())
    );
    assert!(
        receiver
            .behaviour()
            .blacklisted_peers()
            .any(|peer| peer == sender.local_peer_id())
    );
}

#[test(tokio::test)]
async fn duplicate_message_from_old_epoch_after_epoch_rotation_is_suppressed() {
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
            .with_peering_degree(PEERING_DEGREE)
            .build()
    });

    receiver.listen().with_memory_addr_external().await;
    sender_a.connect_and_wait_for_upgrade(&mut receiver).await;
    sender_b.connect_and_wait_for_upgrade(&mut receiver).await;

    // Sender A sends message X. Receiver processes it and stores X as
    // `Processed` in its current-epoch message cache.
    let test_message = TestEncapsulatedMessage::new(b"msg");
    sender_a
        .behaviour_mut()
        .publish_message_with_validated_header_to_current_epoch(test_message.as_ref())
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

    // Receiver starts a new epoch. The message cache now containing X
    // as `Processed` is transferred into the old epoch object,
    // alongside the connections to both sender_a and sender_b.
    let memberships = build_memberships(&[&sender_a, &sender_b, &receiver]);
    receiver.behaviour_mut().start_new_epoch(
        (memberships[2].clone(), 1.into()),
        TestProofsVerifier::accepting(),
    );

    // Sender B sends the identical message X through its (still-open)
    // connection to receiver. From receiver's point of view this connection
    // now belongs to the old epoch. Because X is already in the transferred
    // cache, receiver must NOT emit a second `Message` event.
    sender_b
        .behaviour_mut()
        .publish_message_with_validated_header_to_current_epoch(test_message.as_ref())
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
        "Receiver must not re-emit a message that was already processed in the previous epoch"
    );
}
