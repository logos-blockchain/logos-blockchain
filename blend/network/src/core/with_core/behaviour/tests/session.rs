use futures::StreamExt as _;
use lb_libp2p::SwarmEvent;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::select;

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
