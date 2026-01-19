use futures::StreamExt as _;
use lb_libp2p::SwarmEvent;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::select;

use crate::core::{
    tests::utils::{SessionBasedMockProofsVerifier, TestEncapsulatedMessageWithSession, TestSwarm},
    with_core::{
        behaviour::{
            Event,
            tests::utils::{
                BehaviourBuilder, SwarmExt as _, build_memberships,
                default_poq_verification_inputs_for_session, new_nodes_with_empty_address,
            },
        },
        error::Error,
    },
};

#[test(tokio::test)]
async fn publish_message() {
    let mut session = 0;
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_poq_verification_inputs(default_poq_verification_inputs_for_session(session))
            .build::<SessionBasedMockProofsVerifier>()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_poq_verification_inputs(default_poq_verification_inputs_for_session(session))
            .build::<SessionBasedMockProofsVerifier>()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start a new session before sending any message through the connection.
    session += 1;
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer.behaviour_mut().start_new_session(
        memberships[0].clone(),
        SessionBasedMockProofsVerifier(session),
    );
    listener.behaviour_mut().start_new_session(
        memberships[1].clone(),
        SessionBasedMockProofsVerifier(session),
    );

    // Send a message but expect [`Error::NoPeers`]
    // because we haven't establish connections for the new session.
    let test_message = TestEncapsulatedMessageWithSession::new(session, b"msg");
    let result = dialer
        .behaviour_mut()
        .validate_and_publish_message(test_message.clone().into());
    assert_eq!(result, Err(Error::NoPeers));

    // Establish a connection for the new session.
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Now we can send the message successfully.
    dialer
        .behaviour_mut()
        .validate_and_publish_message(test_message.clone().into())
        .unwrap();
    loop {
        select! {
            _ = dialer.select_next_some() => {}
            event = listener.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message(message, _)) = event {
                    assert_eq!(message.id(), test_message.id());
                    break;
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn forward_message() {
    let old_session = 0;
    let (mut identities, nodes) = new_nodes_with_empty_address(4);
    let mut sender = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_poq_verification_inputs(default_poq_verification_inputs_for_session(old_session))
            .build::<SessionBasedMockProofsVerifier>()
    });
    let mut forwarder = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(2..=2)
            .with_poq_verification_inputs(default_poq_verification_inputs_for_session(old_session))
            .build::<SessionBasedMockProofsVerifier>()
    });
    let mut receiver1 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_poq_verification_inputs(default_poq_verification_inputs_for_session(old_session))
            .build::<SessionBasedMockProofsVerifier>()
    });
    let mut receiver2 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_poq_verification_inputs(default_poq_verification_inputs_for_session(old_session))
            .build::<SessionBasedMockProofsVerifier>()
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
    forwarder.behaviour_mut().start_new_session(
        memberships[1].clone(),
        SessionBasedMockProofsVerifier(new_session),
    );
    receiver1.behaviour_mut().start_new_session(
        memberships[2].clone(),
        SessionBasedMockProofsVerifier(new_session),
    );
    receiver2.behaviour_mut().start_new_session(
        memberships[3].clone(),
        SessionBasedMockProofsVerifier(new_session),
    );
    forwarder.connect_and_wait_for_upgrade(&mut receiver2).await;

    // The sender publishes a message built with the old session to the forwarder.
    let test_message = TestEncapsulatedMessageWithSession::new(old_session, b"msg");
    sender
        .behaviour_mut()
        .validate_and_publish_message(test_message.clone().into())
        .unwrap();

    // We expect that the message goes through the forwarder and receiver1
    // even though the forwarder is connected to the receiver2 in the new session.
    loop {
        select! {
            _ = sender.select_next_some() => {}
            event = forwarder.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message(message, conn)) = event {
                    assert_eq!(message.id(), test_message.id());
                    forwarder.behaviour_mut()
                        .validate_and_forward_message(test_message.clone().into(), conn)
                        .unwrap();
                }
            }
            event = receiver1.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message(message, _)) = event {
                    assert_eq!(message.id(), test_message.id());
                    break;
                }
            }
            _ = receiver2.select_next_some() => {}
        }
    }

    // Now we start the new session for the sender as well.
    // Also, connect the sender to the forwarder for the new session.
    sender.behaviour_mut().start_new_session(
        memberships[0].clone(),
        SessionBasedMockProofsVerifier(new_session),
    );
    sender.connect_and_wait_for_upgrade(&mut forwarder).await;

    // The sender publishes a new message built with the new session to the
    // forwarder.
    let test_message = TestEncapsulatedMessageWithSession::new(new_session, b"msg");
    sender
        .behaviour_mut()
        .validate_and_publish_message(test_message.clone().into())
        .unwrap();

    // We expect that the message goes through the forwarder and receiver2.
    loop {
        select! {
            _ = sender.select_next_some() => {}
            event = forwarder.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message(message, conn)) = event {
                    assert_eq!(message.id(), test_message.id());
                    forwarder.behaviour_mut()
                        .validate_and_forward_message(test_message.clone().into(), conn)
                        .unwrap();
                }
            }
            _ = receiver1.select_next_some() => {}
            event = receiver2.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message(message, _)) = event {
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
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_poq_verification_inputs(default_poq_verification_inputs_for_session(session))
            .build::<SessionBasedMockProofsVerifier>()
    });
    let mut listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_poq_verification_inputs(default_poq_verification_inputs_for_session(session))
            .build::<SessionBasedMockProofsVerifier>()
    });

    listener.listen().with_memory_addr_external().await;
    dialer.connect_and_wait_for_upgrade(&mut listener).await;

    // Start a new session.
    session += 1;
    let memberships = build_memberships(&[&dialer, &listener]);
    dialer.behaviour_mut().start_new_session(
        memberships[0].clone(),
        SessionBasedMockProofsVerifier(session),
    );
    listener.behaviour_mut().start_new_session(
        memberships[1].clone(),
        SessionBasedMockProofsVerifier(session),
    );

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
