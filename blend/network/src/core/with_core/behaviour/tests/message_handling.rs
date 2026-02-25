use core::time::Duration;
use std::collections::HashSet;

use futures::StreamExt as _;
use lb_blend_message::encap::encapsulated::EncapsulatedMessage;
use lb_libp2p::SwarmEvent;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::{AlwaysTrueVerifier, TestEncapsulatedMessage, TestSwarm},
    with_core::{
        behaviour::{
            Event, NegotiatedPeerState, SpamReason,
            tests::utils::{BehaviourBuilder, SwarmExt as _, new_nodes_with_empty_address},
        },
        error::Error,
    },
};

#[test(tokio::test)]
async fn message_sending_and_reception() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
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
        .validate_and_publish_message(test_message.clone().into())
        .unwrap();

    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message(encapsulated_message, (peer_id, _))) = listening_event {
                    assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                    assert_eq!(*encapsulated_message, EncapsulatedMessage::from(test_message.clone()).verify_public_header(&AlwaysTrueVerifier).unwrap());
                    break;
                }
            }
        }
    }

    assert_eq!(
        dialing_swarm
            .behaviour()
            .exchanged_message_identifiers
            .get(listening_swarm.local_peer_id())
            .unwrap()
            .keys()
            .copied()
            .collect::<HashSet<_>>(),
        vec![test_message_id].into_iter().collect::<HashSet<_>>()
    );
}

#[test(tokio::test)]
async fn invalid_public_header_message_publish() {
    let mut dialing_swarm =
        TestSwarm::new_ephemeral(|id| BehaviourBuilder::new(id).build::<AlwaysTrueVerifier>());

    let invalid_signature_message = TestEncapsulatedMessage::new_with_invalid_signature(b"data");
    assert_eq!(
        dialing_swarm
            .behaviour_mut()
            .validate_and_publish_message(invalid_signature_message.into_inner().into()),
        Err(Error::InvalidMessage)
    );
}

#[ignore = "TODO: enable this logic after investigating session/epoch transition issues"]
#[test(tokio::test)]
async fn undeserializable_message_received() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
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

#[ignore = "TODO: enable this logic after investigating session/epoch transition issues"]
#[test(tokio::test)]
async fn duplicate_message_received() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    let test_message = TestEncapsulatedMessage::new(b"msg");
    dialing_swarm
        .behaviour_mut()
        .validate_and_publish_message(test_message.clone().into())
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
                if let SwarmEvent::Behaviour(Event::Message(..)) = listening_event {
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
        .force_send_message_to_peer(&test_message, *listening_swarm.local_peer_id())
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
async fn duplicate_message_within_sensitivity_interval_is_not_spam() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // The dialer publishes a message, which records it in the dialer's
    // exchanged message cache for the listener peer.
    let test_message = TestEncapsulatedMessage::new(b"msg");
    dialing_swarm
        .behaviour_mut()
        .validate_and_publish_message(test_message.clone().into())
        .unwrap();

    // Without any delay, the listener sends the same message back to the
    // dialer. This simulates a race condition where both peers independently
    // forward the same message to each other near-simultaneously. Because the
    // duplicate arrives within the `SENSITIVITY_INTERVAL_FOR_DUPLICATES`, the
    // dialer should silently drop it without flagging the listener as malicious.
    listening_swarm
        .behaviour_mut()
        .force_send_message_to_peer(&test_message, *dialing_swarm.local_peer_id())
        .unwrap();

    loop {
        select! {
            () = sleep(Duration::from_secs(1)) => {
                break;
            }
            _ = dialing_swarm.select_next_some() => {}
            _ = listening_swarm.select_next_some() => {}
        }
    }

    assert_eq!(
        dialing_swarm
            .behaviour()
            .negotiated_peers()
            .get(listening_swarm.local_peer_id())
            .unwrap()
            .negotiated_state,
        NegotiatedPeerState::Healthy
    );
    assert_eq!(
        dialing_swarm
            .behaviour()
            .exchanged_message_identifiers
            .get(listening_swarm.local_peer_id())
            .unwrap()
            .keys()
            .next()
            .unwrap(),
        &test_message.id()
    );
    assert_eq!(
        listening_swarm
            .behaviour()
            .negotiated_peers()
            .get(dialing_swarm.local_peer_id())
            .unwrap()
            .negotiated_state,
        NegotiatedPeerState::Healthy
    );
    assert_eq!(
        listening_swarm
            .behaviour()
            .exchanged_message_identifiers
            .get(dialing_swarm.local_peer_id())
            .unwrap()
            .keys()
            .next()
            .unwrap(),
        &test_message.id()
    );
}

#[ignore = "TODO: enable this logic after investigating session/epoch transition issues"]
#[test(tokio::test)]
async fn invalid_public_header_message_received() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .build::<AlwaysTrueVerifier>()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    let invalid_public_header_message = TestEncapsulatedMessage::new_with_invalid_signature(b"");
    dialing_swarm
        .behaviour_mut()
        .force_send_message_to_peer(
            &invalid_public_header_message,
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
                        assert_eq!(peer_state, NegotiatedPeerState::Spammy(SpamReason::InvalidPublicHeader));
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
