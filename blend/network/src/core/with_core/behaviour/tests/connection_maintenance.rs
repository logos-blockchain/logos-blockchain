use core::time::Duration;

use futures::StreamExt as _;
use lb_libp2p::SwarmEvent;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::{TestEncapsulatedMessage, TestSwarm},
    with_core::behaviour::{
        Event, NegotiatedPeerState, SpamReason,
        tests::utils::{
            BehaviourBuilder, IntervalProviderBuilder, SwarmExt as _, new_nodes_with_empty_address,
        },
    },
};

#[ignore = "TODO: enable this logic after investigating session/epoch transition issues. Test disabled because we don't let connections turn spammy because of too many messages now until we have proper observation window values."]
#[test(tokio::test)]
async fn detect_spammy_peer() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_provider(IntervalProviderBuilder::default().with_range(1..=1).build())
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // We let the first observation clock tick.
    sleep(Duration::from_secs(2)).await;

    // Send two messages when only one was expected.
    dialing_swarm
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &TestEncapsulatedMessage::new(b"msg1").into_inner().into(),
        )
        .unwrap();
    dialing_swarm
        .behaviour_mut()
        .publish_message_with_validated_signature_to_current_session(
            &TestEncapsulatedMessage::new(b"msg2").into_inner().into(),
        )
        .unwrap();

    let mut events_to_match = 2u8;
    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                match listening_event {
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, NegotiatedPeerState::Spammy(SpamReason::TooManyMessages))) => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert!(listening_swarm.behaviour().negotiated_peers.is_empty());
                        assert!(listening_swarm.behaviour().message_cache.messages_from_peer(dialing_swarm.local_peer_id()).count() == 2);
                        events_to_match -= 1;
                    }
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                        assert!(listening_swarm.behaviour().message_cache.messages_from_peer(dialing_swarm.local_peer_id()).count() == 0);
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

#[ignore = "TODO: enable this logic after investigating session/epoch transition issues. Test disabled because we don't let connections turn unhealthy now until we have proper observation window values."]
#[test(tokio::test)]
async fn detect_unhealthy_peer() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_provider(IntervalProviderBuilder::default().with_range(1..=1).build())
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // Do not send any message from dialing to listening swarm.

    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::UnhealthyPeer(peer_id)) = listening_event {
                    assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                    break;
                }
            }
        }
    }

    // We make sure that the same "Unhealthy" notification is not bubbled up to the
    // swarm again by the behaviour for an already unhealthy peer.

    loop {
        select! {
            () = sleep(Duration::from_secs(5)) => {
                break;
            }
            _ = dialing_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::UnhealthyPeer(peer_id)) = listening_event {
                    assert!(peer_id != *dialing_swarm.local_peer_id());
                }
            }
        }
    }

    assert!(
        listening_swarm
            .behaviour()
            .negotiated_peers
            .get(dialing_swarm.local_peer_id())
            .unwrap()
            .negotiated_state
            .is_unhealthy()
    );
}

#[ignore = "TODO: enable this logic after investigating session/epoch transition issues. Test disabled because we don't let connections turn unhealthy now until we have proper observation window values."]
#[test(tokio::test)]
async fn restore_healthy_peer() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_provider(IntervalProviderBuilder::default().with_range(1..=1).build())
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // Let the connection turn unhealthy.
    sleep(Duration::from_secs(4)).await;

    // Send a message to the listening swarm to revert from unhealthy to healthy.
    dialing_swarm
        .behaviour_mut()
        .force_send_message_to_current_session_peer(
            &TestEncapsulatedMessage::new(b"msg").into_inner(),
            *listening_swarm.local_peer_id(),
        )
        .unwrap();

    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::HealthyPeer(peer_id)) = listening_event {
                    assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                    break;
                }
            }
        }
    }

    assert!(
        listening_swarm
            .behaviour()
            .negotiated_peers
            .get(dialing_swarm.local_peer_id())
            .unwrap()
            .negotiated_state
            .is_healthy()
    );
}
