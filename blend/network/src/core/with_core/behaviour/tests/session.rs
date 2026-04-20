use core::time::Duration;

use futures::StreamExt as _;
use lb_libp2p::SwarmEvent;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::{TestEncapsulatedMessageWithSession, TestSwarm},
    with_core::{
        behaviour::{
            Event,
            tests::utils::{
                BehaviourBuilder, SwarmExt as _, build_memberships, new_nodes_with_empty_address,
            },
        },
        error::SendError,
    },
};

#[test(tokio::test)]
async fn publish_message() {
    let mut session = 0;
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start a new session before sending any message through the connection.
    session += 1;
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), session));
    listener
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), session));

    // Send a message but expect [`Error::NoPeers`]
    // because we haven't establish connections for the new session.
    let test_message = TestEncapsulatedMessageWithSession::new(session, b"msg");
    let result = dialer
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), session);
    assert_eq!(result, Err(SendError::NoPeers));

    // Establish a connection for the new session.
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Now we can send the message successfully.
    dialer
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), session)
        .unwrap();
    loop {
        select! {
            _ = dialer.select_next_some() => {}
            event = listener.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, .. }) = event {
                    assert_eq!(message.id(), test_message.id());
                    break;
                }
            }
        }
    }

    // We cannot send the same message again because it's already processed.
    assert_eq!(
        dialer
            .behaviour_mut()
            .publish_message_with_validated_header(test_message.clone(), 1),
        Err(SendError::DuplicateMessage)
    );
}

#[test(tokio::test)]
async fn forward_message() {
    let old_session = 0;
    let (mut identities, nodes) = new_nodes_with_empty_address(4);
    let mut sender = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut forwarder = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(2..=2)
            .build()
    });
    let mut receiver1 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut receiver2 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    forwarder.listen().with_memory_addr_external().await;
    receiver1.listen().with_memory_addr_external().await;
    receiver2.listen().with_memory_addr_external().await;

    // Connect 3 nodes: sender -> forwarder -> receiver1
    sender.connect_and_wait_for_upgrade(&mut forwarder).await;
    forwarder.connect_and_wait_for_upgrade(&mut receiver1).await;

    // Before sending any message, start a new session
    // only for the forwarder, receiver1, and receiver2.
    // And, connect the forwarder to the receiver2 for the new session.
    // Then, the topology looks like:
    // - Old session: sender -> forwarder -> receiver1
    // - New session:           forwarder -> receiver2
    let new_session = old_session + 1;
    let memberships = build_memberships(&[&sender, &forwarder, &receiver1, &receiver2]);
    forwarder
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), new_session));
    receiver1
        .behaviour_mut()
        .start_new_session((memberships[2].clone(), new_session));
    receiver2
        .behaviour_mut()
        .start_new_session((memberships[3].clone(), new_session));
    forwarder.connect_and_wait_for_upgrade(&mut receiver2).await;

    // The sender publishes a message built with the old session to the forwarder.
    let test_message = TestEncapsulatedMessageWithSession::new(old_session, b"msg");
    sender
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), old_session)
        .unwrap();

    // We expect that the message goes through the forwarder and receiver1
    // even though the forwarder is connected to the receiver2 in the new session.
    loop {
        select! {
            _ = sender.select_next_some() => {}
            event = forwarder.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, session, sender }) = event {
                    assert_eq!(message.id(), test_message.id());
                    forwarder.behaviour_mut()
                        .forward_message_with_validated_signature(&message, sender, session)
                        .unwrap();
                }
            }
            event = receiver1.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, .. }) = event {
                    assert_eq!(message.id(), test_message.id());
                    break;
                }
            }
            _ = receiver2.select_next_some() => {}
        }
    }

    // Now we start the new session for the sender as well.
    // Also, connect the sender to the forwarder for the new session.
    sender
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), new_session));
    sender.connect_and_wait_for_upgrade(&mut forwarder).await;

    // The sender publishes a new message built with the new session to the
    // forwarder.
    let test_message = TestEncapsulatedMessageWithSession::new(new_session, b"msg");
    sender
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), new_session)
        .unwrap();

    // We expect that the message goes through the forwarder and receiver2.
    loop {
        select! {
            _ = sender.select_next_some() => {}
            event = forwarder.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, session, sender }) = event {
                    assert_eq!(message.id(), test_message.id());
                    forwarder.behaviour_mut()
                        .forward_message_with_validated_signature(&message, sender, session)
                        .unwrap();
                }
            }
            _ = receiver1.select_next_some() => {}
            event = receiver2.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, .. }) = event {
                    assert_eq!(message.id(), test_message.id());
                    break;
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn finish_session_transition() {
    let mut session = 0;
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start a new session.
    session += 1;
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), session));
    listener
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), session));

    // Finish the transition period
    dialer.behaviour_mut().finish_session_transition();
    listener.behaviour_mut().finish_session_transition();

    // Expect that the connection is closed after 10s (default swarm timeout).
    loop {
        select! {
            _ = dialer.select_next_some() => {}
            event = listener.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { .. } = event {
                    break;
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn old_session_message_not_forwarded_back_to_sender() {
    let old_session = 0;
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let mut sender = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut forwarder = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(2..=2)
            .build()
    });
    let mut receiver = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    forwarder.listen().with_memory_addr_external().await;
    receiver.listen().with_memory_addr_external().await;
    // Topology: sender -> forwarder -> receiver (current session).
    sender.connect_and_wait_for_upgrade(&mut forwarder).await;
    forwarder.connect_and_wait_for_upgrade(&mut receiver).await;

    // Forwarder starts a new session. Both the sender and the receiver
    // connections move into the forwarder's old session.
    let new_session = old_session + 1;
    let memberships = build_memberships(&[&sender, &forwarder, &receiver]);
    forwarder
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), new_session));

    // Sender publishes a message for the old session.
    let test_message = TestEncapsulatedMessageWithSession::new(old_session, b"msg");
    sender
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), old_session)
        .unwrap();

    // Forwarder receives the message via the old session and forwards it,
    // excluding the original sender. Only receiver should receive the message.
    loop {
        select! {
            _ = sender.select_next_some() => {}
            forwarder_event = forwarder.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, session, sender: msg_sender }) = forwarder_event {
                    assert_eq!(message.id(), test_message.id());
                    forwarder.behaviour_mut()
                        .forward_message_with_validated_signature(&message, msg_sender, session)
                        .unwrap();
                }
            }
            receiver_event = receiver.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, .. }) = receiver_event {
                    assert_eq!(message.id(), test_message.id());
                    break;
                }
            }
        }
    }

    // After receiver confirmed receipt, poll for a while to ensure sender
    // does not receive the message back from the forwarder.
    let mut sender_received_message_back = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(3)) => { break; }
            _ = receiver.select_next_some() => {}
            _ = forwarder.select_next_some() => {}
            sender_event = sender.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { .. }) = sender_event {
                    sender_received_message_back = true;
                }
            }
        }
    }

    assert!(
        !sender_received_message_back,
        "Old session should not forward the message back to the original sender"
    );
}

#[test(tokio::test)]
async fn publish_to_invalid_session_returns_error() {
    let session = 1;
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start the first session and connect.
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), session));
    listener
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), session));
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Attempt to publish to a session that neither matches the current nor old.
    let test_message = TestEncapsulatedMessageWithSession::new(999, b"invalid-session");
    let result = dialer
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), 999);
    assert_eq!(result, Err(SendError::InvalidSession));
}

#[test(tokio::test)]
async fn forward_to_invalid_session_returns_error() {
    let session = 1;
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start the first session and connect.
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), session));
    listener
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), session));
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Attempt to forward a message to an invalid session.
    let test_message = TestEncapsulatedMessageWithSession::new(999, b"invalid-session");
    let fake_sender = *listener.local_peer_id();
    let sig_verified: lb_blend_message::encap::validated::EncapsulatedMessageWithVerifiedSignature =
        (*test_message).clone().into();
    let result = dialer
        .behaviour_mut()
        .forward_message_with_validated_signature(&sig_verified, fake_sender, 999);
    assert_eq!(result, Err(SendError::InvalidSession));
}

#[test(tokio::test)]
async fn event_message_carries_session_number() {
    let mut session = 0;
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start session 1 and connect.
    session += 1;
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), session));
    listener
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), session));
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Send a message for session 1.
    let test_message = TestEncapsulatedMessageWithSession::new(session, b"session-check");
    dialer
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), session)
        .unwrap();

    loop {
        select! {
            _ = dialer.select_next_some() => {}
            event = listener.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, session: event_session, .. }) = event {
                    assert_eq!(message.id(), test_message.id());
                    assert_eq!(event_session, session, "Event::Message must carry the correct session number");
                    break;
                }
            }
        }
    }
}

/// After `start_new_session()`, current `negotiated_peers` must be empty and
/// old peers must live inside the `OldSession`.
#[test(tokio::test)]
async fn start_new_session_moves_peers_to_old_session() {
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let mut node_a = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(2..=2)
            .build()
    });
    let mut node_b = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut node_c = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    node_b.listen().with_memory_addr_external().await;
    node_c.listen().with_memory_addr_external().await;
    node_a.connect_and_wait_for_upgrade(&mut node_b).await;
    node_a.connect_and_wait_for_upgrade(&mut node_c).await;

    // Before session transition: node_a has 2 negotiated peers.
    assert_eq!(node_a.behaviour().negotiated_peers.len(), 2);
    assert!(node_a.behaviour().old_session.is_none());

    // Start a new session.
    let memberships = build_memberships(&[&node_a, &node_b, &node_c]);
    node_a
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), 1));

    // After session transition: current negotiated_peers must be empty
    // and old_session must exist.
    assert_eq!(
        node_a.behaviour().negotiated_peers.len(),
        0,
        "Current session negotiated peers must be reset after start_new_session"
    );
    assert!(
        node_a.behaviour().old_session.is_some(),
        "Old session must be created after start_new_session"
    );
}

/// `finish_session_transition()` emits close substream events for all peers
/// in the old session, generating `PeerDisconnected` events.
#[test(tokio::test)]
async fn finish_session_transition_emits_peer_disconnected_for_old_session_peers() {
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let mut node_a = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(2..=2)
            .build()
    });
    let mut node_b = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut node_c = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    node_b.listen().with_memory_addr_external().await;
    node_c.listen().with_memory_addr_external().await;
    node_a.connect_and_wait_for_upgrade(&mut node_b).await;
    node_a.connect_and_wait_for_upgrade(&mut node_c).await;

    // Start a new session to move current peers into old session.
    let memberships = build_memberships(&[&node_a, &node_b, &node_c]);
    node_a
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), 1));

    // Finish the transition; this should close all old session connections.
    node_a.behaviour_mut().finish_session_transition();

    // Old session should now be gone.
    assert!(
        node_a.behaviour().old_session.is_none(),
        "Old session must be cleared after finish_session_transition"
    );

    // Drive the swarms until both connections from the old session close.
    let mut closed_count = 0usize;
    loop {
        select! {
            _ = node_b.select_next_some() => {}
            _ = node_c.select_next_some() => {}
            event = node_a.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { .. } = event {
                    closed_count += 1;
                    if closed_count >= 2 {
                        break;
                    }
                }
            }
            () = sleep(Duration::from_secs(15)) => {
                panic!("Timed out waiting for old session connections to close");
            }
        }
    }
}

/// Multiple consecutive `start_new_session` calls should discard the previous
/// old session, moving current peers into a new old session each time.
#[test(tokio::test)]
async fn consecutive_session_transitions_replace_old_session() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // First session transition: move the current peer into old session.
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), 1));
    listener
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), 1));
    assert!(dialer.behaviour().old_session.is_some());

    // Re-establish a connection for session 1 so there is something to move
    // into old session again.
    dialer.connect_and_wait_for_upgrade(&mut listener).await;
    assert_eq!(dialer.behaviour().negotiated_peers.len(), 1);

    // Second session transition: old session from session 0 gets stopped
    // and current peers from session 1 move into old session.
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), 2));
    listener
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), 2));
    assert!(dialer.behaviour().old_session.is_some());
    assert_eq!(
        dialer.behaviour().negotiated_peers.len(),
        0,
        "Current negotiated peers must be empty after session transition"
    );

    // Drive the swarm until the original (session 0) connection closes
    // (due to stop_old_session called for session 0 peers inside the second
    // start_new_session).
    loop {
        select! {
            _ = listener.select_next_some() => {}
            event = dialer.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { .. } = event {
                    break;
                }
            }
            () = sleep(Duration::from_secs(15)) => {
                panic!("Timed out waiting for old session 0 connections to close");
            }
        }
    }
}

/// Verify that after session transition, re-bootstrapping into the new session
/// respects peering degree limits.
#[test(tokio::test)]
async fn session_transition_reboots_peering_degree() {
    let (mut identities, nodes) = new_nodes_with_empty_address(4);
    let mut node_a = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            // Peering degree: exactly 2 peers.
            .with_peering_degree(2..=2)
            .build()
    });
    let mut node_b = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut node_c = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut node_d = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    node_b.listen().with_memory_addr_external().await;
    node_c.listen().with_memory_addr_external().await;
    node_d.listen().with_memory_addr_external().await;

    // Connect node_a to b and c (filling peering degree of 2).
    node_a.connect_and_wait_for_upgrade(&mut node_b).await;
    node_a.connect_and_wait_for_upgrade(&mut node_c).await;
    assert_eq!(node_a.behaviour().negotiated_peers.len(), 2);
    assert_eq!(node_a.behaviour().available_connection_slots(), 0);

    // Start session transition - all current peers move to old session.
    let memberships = build_memberships(&[&node_a, &node_b, &node_c, &node_d]);
    node_a
        .behaviour_mut()
        .start_new_session((memberships[0].clone(), 1));
    node_b
        .behaviour_mut()
        .start_new_session((memberships[1].clone(), 1));
    node_c
        .behaviour_mut()
        .start_new_session((memberships[2].clone(), 1));
    node_d
        .behaviour_mut()
        .start_new_session((memberships[3].clone(), 1));

    // After transition, new session has no peers, so all slots are available.
    assert_eq!(
        node_a.behaviour().available_connection_slots(),
        2,
        "All peering degree slots must be available after session transition"
    );

    // Connect to new peers in the new session.
    node_a.connect_and_wait_for_upgrade(&mut node_b).await;
    node_a.connect_and_wait_for_upgrade(&mut node_d).await;
    assert_eq!(node_a.behaviour().negotiated_peers.len(), 2);
    assert_eq!(node_a.behaviour().available_connection_slots(), 0);
}
