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
            tests::utils::{BehaviourBuilder, SwarmExt as _, new_nodes_with_empty_address},
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
