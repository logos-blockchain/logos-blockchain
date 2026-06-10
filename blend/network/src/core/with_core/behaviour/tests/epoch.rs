use core::time::Duration;

use either::Either;
use futures::StreamExt as _;
use lb_blend_scheduling::membership::Membership;
use lb_cryptarchia_engine::Epoch;
use lb_libp2p::{NetworkBehaviour as _, SwarmEvent};
use libp2p::{Multiaddr, swarm::ConnectionId};
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::{TestEncapsulatedMessageWithEpoch, TestSwarm},
    with_core::{
        behaviour::{
            Event,
            handler::ToBehaviour,
            tests::utils::{
                BehaviourBuilder, SwarmExt as _, build_memberships, new_nodes_with_empty_address,
            },
        },
        error::SendError,
    },
};

#[test(tokio::test)]
async fn publish_message() {
    let mut epoch = Epoch::new(0);
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start a new epoch before sending any message through the connection.
    epoch = Epoch::new(epoch.into_inner() + 1);
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), epoch));
    listener
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), epoch));

    // Send a message but expect [`Error::NoPeers`]
    // because we haven't establish connections for the new epoch.
    let test_message = TestEncapsulatedMessageWithEpoch::new(epoch, b"msg");
    let result = dialer
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), epoch);
    assert_eq!(result, Err(SendError::NoPeers));

    // Establish a connection for the new epoch.
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Now we can send the message successfully.
    dialer
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), epoch)
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
            .publish_message_with_validated_header(test_message.clone(), 1.into()),
        Err(SendError::DuplicateMessage)
    );
}

/// Regression test for audit finding #2: a `FullyNegotiated` event that races
/// an epoch transition must be ignored, not panic the blend swarm task.
///
/// Handler→behaviour events are delivered asynchronously, so a handler can emit
/// `FullyNegotiated` *before* `start_new_epoch` clears
/// `connections_waiting_upgrade`, with the event delivered *after*:
///
/// ```text
///   handler.poll() ──emit FullyNegotiated(conn)──┐  (queued toward swarm)
///                                                 │
///   service loop: StartNewEpoch                   │
///     └─ start_new_epoch() ─ mem::take(waiting) ──┤  conn entry now gone
///                                                 ▼
///   swarm delivers FullyNegotiated(conn) ─► on_connection_handler_event
///     └─ handle_negotiated_connection(conn)
///          └─ waiting.remove(conn) == None ─► ignored (no panic) ✓
/// ```
///
/// This drives the behaviour at the `NetworkBehaviour` API level to land it in
/// exactly that post-race state deterministically: accept an inbound connection
/// (which inserts into `connections_waiting_upgrade`), run `start_new_epoch`
/// (which empties the map), then deliver the in-flight `FullyNegotiated`. The
/// stale event must be dropped silently, leaving no negotiated peer behind.
#[test(tokio::test)]
async fn fully_negotiated_racing_epoch_transition_is_ignored() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let local_identity = identities.next().unwrap();
    // The remote must be a core member so the inbound connection is upgraded
    // (and thus recorded in `connections_waiting_upgrade`).
    let peer_id = nodes[1].id;

    let mut behaviour = BehaviourBuilder::new(&local_identity)
        .with_membership(&nodes)
        .build();

    // A connection is established and we choose to upgrade it: this records an
    // entry in `connections_waiting_upgrade`, and the handler will (eventually)
    // emit `FullyNegotiated` for it.
    let connection_id = ConnectionId::new_unchecked(0);
    let addr = Multiaddr::empty();
    let _handler = behaviour
        .handle_established_inbound_connection(connection_id, peer_id, &addr, &addr)
        .expect("inbound connection with a core peer should be accepted");
    assert!(
        behaviour
            .connections_waiting_upgrade
            .contains_key(&(peer_id, connection_id)),
        "the established connection must be pending upgrade"
    );

    // The epoch transition fires while the handler's `FullyNegotiated` is still
    // in flight. `start_new_epoch` clears the pending-upgrade map.
    behaviour.start_new_epoch((Membership::new_without_local(&[]), Epoch::new(1)));
    assert!(
        behaviour.connections_waiting_upgrade.is_empty(),
        "start_new_epoch must clear the pending-upgrade map"
    );

    // The previously-emitted `FullyNegotiated` is now delivered. The connection
    // is no longer pending upgrade, so the stale event must be ignored instead
    // of panicking the swarm task.
    behaviour.on_connection_handler_event(
        peer_id,
        connection_id,
        Either::Left(ToBehaviour::FullyNegotiated),
    );

    // The stale event left no trace: the peer was not promoted to a negotiated
    // peer in the new epoch.
    assert!(
        !behaviour.negotiated_peers.contains_key(&peer_id),
        "A stale FullyNegotiated must not promote the peer in the new epoch"
    );
}

#[test(tokio::test)]
async fn forward_message() {
    let old_epoch = Epoch::new(0);
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

    // Before sending any message, start a new epoch
    // only for the forwarder, receiver1, and receiver2.
    // And, connect the forwarder to the receiver2 for the new epoch.
    // Then, the topology looks like:
    // - Old epoch: sender -> forwarder -> receiver1
    // - New epoch:           forwarder -> receiver2
    let new_epoch = Epoch::new(old_epoch.into_inner() + 1);
    let memberships = build_memberships(&[&sender, &forwarder, &receiver1, &receiver2]);
    forwarder
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), new_epoch));
    receiver1
        .behaviour_mut()
        .start_new_epoch((memberships[2].clone(), new_epoch));
    receiver2
        .behaviour_mut()
        .start_new_epoch((memberships[3].clone(), new_epoch));
    forwarder.connect_and_wait_for_upgrade(&mut receiver2).await;

    // The sender publishes a message built with the old epoch to the forwarder.
    let test_message = TestEncapsulatedMessageWithEpoch::new(old_epoch, b"msg");
    sender
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), old_epoch)
        .unwrap();

    // We expect that the message goes through the forwarder and receiver1
    // even though the forwarder is connected to the receiver2 in the new epoch.
    loop {
        select! {
            _ = sender.select_next_some() => {}
            event = forwarder.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, epoch, sender }) = event {
                    assert_eq!(message.id(), test_message.id());
                    forwarder.behaviour_mut()
                        .forward_message_with_validated_signature(&message, sender, epoch)
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

    // Now we start the new epoch for the sender as well.
    // Also, connect the sender to the forwarder for the new epoch.
    sender
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), new_epoch));
    sender.connect_and_wait_for_upgrade(&mut forwarder).await;

    // The sender publishes a new message built with the new epoch to the
    // forwarder.
    let test_message = TestEncapsulatedMessageWithEpoch::new(new_epoch, b"msg");
    sender
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), new_epoch)
        .unwrap();

    // We expect that the message goes through the forwarder and receiver2.
    loop {
        select! {
            _ = sender.select_next_some() => {}
            event = forwarder.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, epoch, sender }) = event {
                    assert_eq!(message.id(), test_message.id());
                    forwarder.behaviour_mut()
                        .forward_message_with_validated_signature(&message, sender, epoch)
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
async fn finish_epoch_transition() {
    let mut epoch = Epoch::new(0);
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start a new epoch.
    epoch = Epoch::new(epoch.into_inner() + 1);
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), epoch));
    listener
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), epoch));

    // Finish the transition period
    dialer.behaviour_mut().finish_epoch_transition();
    listener.behaviour_mut().finish_epoch_transition();

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
async fn old_epoch_message_not_forwarded_back_to_sender() {
    let old_epoch = Epoch::new(0);
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
    // Topology: sender -> forwarder -> receiver (current epoch).
    sender.connect_and_wait_for_upgrade(&mut forwarder).await;
    forwarder.connect_and_wait_for_upgrade(&mut receiver).await;

    // Forwarder starts a new epoch. Both the sender and the receiver
    // connections move into the forwarder's old epoch.
    let new_epoch = Epoch::new(old_epoch.into_inner() + 1);
    let memberships = build_memberships(&[&sender, &forwarder, &receiver]);
    forwarder
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), new_epoch));

    // Sender publishes a message for the old epoch.
    let test_message = TestEncapsulatedMessageWithEpoch::new(old_epoch, b"msg");
    sender
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), old_epoch)
        .unwrap();

    // Forwarder receives the message via the old epoch and forwards it,
    // excluding the original sender. Only receiver should receive the message.
    loop {
        select! {
            _ = sender.select_next_some() => {}
            forwarder_event = forwarder.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, epoch, sender: msg_sender }) = forwarder_event {
                    assert_eq!(message.id(), test_message.id());
                    forwarder.behaviour_mut()
                        .forward_message_with_validated_signature(&message, msg_sender, epoch)
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
        "Old epoch should not forward the message back to the original sender"
    );
}

#[test(tokio::test)]
async fn publish_to_invalid_epoch_returns_error() {
    let epoch = Epoch::new(1);
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start the first epoch and connect.
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), epoch));
    listener
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), epoch));
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Attempt to publish to an epoch that neither matches the current nor old.
    let test_message = TestEncapsulatedMessageWithEpoch::new(999.into(), b"invalid-epoch");
    let result = dialer
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), 999.into());
    assert_eq!(result, Err(SendError::InvalidEpoch));
}

#[test(tokio::test)]
async fn forward_to_invalid_epoch_returns_error() {
    let epoch = Epoch::new(1);
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start the first epoch and connect.
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), epoch));
    listener
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), epoch));
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Attempt to forward a message to an invalid epoch.
    let test_message = TestEncapsulatedMessageWithEpoch::new(999.into(), b"invalid-epoch");
    let fake_sender = *listener.local_peer_id();
    let sig_verified: lb_blend_message::encap::validated::EncapsulatedMessageWithVerifiedSignature =
        (*test_message).clone().into();
    let result = dialer
        .behaviour_mut()
        .forward_message_with_validated_signature(&sig_verified, fake_sender, 999.into());
    assert_eq!(result, Err(SendError::InvalidEpoch));
}

#[test(tokio::test)]
async fn event_message_carries_epoch_number() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start epoch 1 and connect.
    let epoch = Epoch::new(1);
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), epoch));
    listener
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), epoch));
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Send a message for epoch 1.
    let test_message = TestEncapsulatedMessageWithEpoch::new(epoch, b"epoch-check");
    dialer
        .behaviour_mut()
        .publish_message_with_validated_header(test_message.clone(), epoch)
        .unwrap();

    loop {
        select! {
            _ = dialer.select_next_some() => {}
            event = listener.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message { message, epoch: event_epoch, .. }) = event {
                    assert_eq!(message.id(), test_message.id());
                    assert_eq!(event_epoch, epoch, "Event::Message must carry the correct epoch number");
                    break;
                }
            }
        }
    }
}

/// After `start_new_epoch()`, current `negotiated_peers` must be empty and
/// old peers must live inside the `OldEpoch`.
#[test(tokio::test)]
async fn start_new_epoch_moves_peers_to_old_epoch() {
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

    // Before epoch transition: node_a has 2 negotiated peers.
    assert_eq!(node_a.behaviour().negotiated_peers.len(), 2);
    assert!(node_a.behaviour().old_epoch.is_none());

    // Start a new epoch.
    let memberships = build_memberships(&[&node_a, &node_b, &node_c]);
    node_a
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), 1.into()));

    // After epoch transition: current negotiated_peers must be empty
    // and old_epoch must exist.
    assert_eq!(
        node_a.behaviour().negotiated_peers.len(),
        0,
        "Current epoch negotiated peers must be reset after start_new_epoch"
    );
    assert!(
        node_a.behaviour().old_epoch.is_some(),
        "Old epoch must be created after start_new_epoch"
    );
}

/// `finish_epoch_transition()` emits close substream events for all peers
/// in the old epoch, generating `PeerDisconnected` events.
#[test(tokio::test)]
async fn finish_epoch_transition_emits_peer_disconnected_for_old_epoch_peers() {
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

    // Start a new epoch to move current peers into old epoch.
    let memberships = build_memberships(&[&node_a, &node_b, &node_c]);
    node_a
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), 1.into()));

    // Finish the transition; this should close all old epoch connections.
    node_a.behaviour_mut().finish_epoch_transition();

    // Old epoch should now be gone.
    assert!(
        node_a.behaviour().old_epoch.is_none(),
        "Old epoch must be cleared after finish_epoch_transition"
    );

    // Drive the swarms until both connections from the old epoch close.
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
                panic!("Timed out waiting for old epoch connections to close");
            }
        }
    }
}

/// Multiple consecutive `start_new_epoch` calls should discard the previous
/// old epoch, moving current peers into a new old epoch each time.
#[test(tokio::test)]
async fn consecutive_epoch_transitions_replace_old_epoch() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // First epoch transition: move the current peer into old epoch.
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), 1.into()));
    listener
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), 1.into()));
    assert!(dialer.behaviour().old_epoch.is_some());

    // Re-establish a connection for epoch 1 so there is something to move
    // into old epoch again.
    dialer.connect_and_wait_for_upgrade(&mut listener).await;
    assert_eq!(dialer.behaviour().negotiated_peers.len(), 1);

    // Second epoch transition: old epoch from epoch 0 gets stopped
    // and current peers from epoch 1 move into old epoch.
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), 2.into()));
    listener
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), 2.into()));
    assert!(dialer.behaviour().old_epoch.is_some());
    assert_eq!(
        dialer.behaviour().negotiated_peers.len(),
        0,
        "Current negotiated peers must be empty after epoch transition"
    );

    // Drive the swarm until the original (epoch 0) connection closes
    // (due to stop_old_epoch called for epoch 0 peers inside the second
    // start_new_epoch).
    loop {
        select! {
            _ = listener.select_next_some() => {}
            event = dialer.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { .. } = event {
                    break;
                }
            }
            () = sleep(Duration::from_secs(15)) => {
                panic!("Timed out waiting for old epoch 0 connections to close");
            }
        }
    }
}

/// Verify that after epoch transition, re-bootstrapping into the new epoch
/// respects peering degree limits.
#[test(tokio::test)]
async fn epoch_transition_reboots_peering_degree() {
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

    // Start epoch transition - all current peers move to old epoch.
    let memberships = build_memberships(&[&node_a, &node_b, &node_c, &node_d]);
    node_a
        .behaviour_mut()
        .start_new_epoch((memberships[0].clone(), 1.into()));
    node_b
        .behaviour_mut()
        .start_new_epoch((memberships[1].clone(), 1.into()));
    node_c
        .behaviour_mut()
        .start_new_epoch((memberships[2].clone(), 1.into()));
    node_d
        .behaviour_mut()
        .start_new_epoch((memberships[3].clone(), 1.into()));

    // After transition, new epoch has no peers, so all slots are available.
    assert_eq!(
        node_a.behaviour().available_connection_slots(),
        2,
        "All peering degree slots must be available after epoch transition"
    );

    // Connect to new peers in the new epoch.
    node_a.connect_and_wait_for_upgrade(&mut node_b).await;
    node_a.connect_and_wait_for_upgrade(&mut node_d).await;
    assert_eq!(node_a.behaviour().negotiated_peers.len(), 2);
    assert_eq!(node_a.behaviour().available_connection_slots(), 0);
}
