use core::time::Duration;
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use lb_libp2p::SwarmEvent;
use libp2p::{PeerId, Stream, swarm::dummy};
use libp2p_stream::{Behaviour as StreamBehaviour, OpenStreamError};
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, spawn, time::sleep};

use crate::core::{
    tests::utils::{PROTOCOL_NAME, TestSwarm},
    with_edge::behaviour::tests::utils::{BehaviourBuilder, StreamBehaviourExt as _},
};

#[test(tokio::test)]
async fn edge_peer_not_supporting_blend() {
    let mut dummy_swarm = TestSwarm::new_ephemeral(|_| dummy::Behaviour);
    let mut blend_swarm = TestSwarm::new_ephemeral(|_| {
        // Add random peer to membership so the dummy swarm is considered an edge node.
        BehaviourBuilder::new(PeerId::random()).build()
    });

    blend_swarm.listen().with_memory_addr_external().await;
    dummy_swarm.connect(&mut blend_swarm).await;

    let mut events_to_match = 2u8;
    loop {
        select! {
            core_event = blend_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = core_event {
                    assert_eq!(peer_id, *dummy_swarm.local_peer_id());
                    assert!(endpoint.is_listener());
                    events_to_match -= 1;
                }
            }
            dummy_event = dummy_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = dummy_event {
                    assert_eq!(peer_id, *blend_swarm.local_peer_id());
                    assert!(endpoint.is_dialer());
                    events_to_match -= 1;
                }
            }
        }
        if events_to_match == 0 {
            break;
        }
    }
}

#[test(tokio::test)]
async fn non_edge_peer() {
    let mut other_core_swarm = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut blend_swarm = TestSwarm::new_ephemeral(|_| {
        BehaviourBuilder::new(*other_core_swarm.local_peer_id()).build()
    });

    blend_swarm.listen().with_memory_addr_external().await;
    other_core_swarm.connect(&mut blend_swarm).await;

    let stream_control_res = other_core_swarm
        .behaviour_mut()
        .new_control()
        .open_stream(*blend_swarm.local_peer_id(), PROTOCOL_NAME)
        .await;
    assert!(matches!(
        stream_control_res,
        Err(OpenStreamError::UnsupportedProtocol(_))
    ));
}

#[test(tokio::test)]
async fn incoming_connection_with_maximum_peering_degree() {
    let mut edge_swarm_1 = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut edge_swarm_2 = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut blend_swarm = TestSwarm::new_ephemeral(|_| {
        // Add random peer to membership so the two swarms are considered edge nodes.
        BehaviourBuilder::new(PeerId::random())
            // We increase the timeout to send a message so that the swarm will close the rejected
            // connection before the behaviour closes the second connection due to inactivity.
            .with_timeout(Duration::from_secs(13))
            .with_max_incoming_connections(1)
            .build()
    });

    blend_swarm.listen().with_memory_addr_external().await;

    // We wait that the first connection is established and upgraded.
    // We keep owning the stream or else the connection is dropped.
    let _stream = edge_swarm_1
        .connect_and_upgrade_to_blend(&mut blend_swarm)
        .await;

    // Then we perform the second dial.
    edge_swarm_2.connect(&mut blend_swarm).await;

    let mut edge_swarm_connection_2_closed = false;
    // We verify that the additional connection is closed, and that the old one is
    // kept alive (since we set a timeout of 13 seconds while the default swarm has
    // a timeout of 10 seconds).
    loop {
        select! {
            () = sleep(Duration::from_secs(12)) => {
                break;
            }
             edge_swarm_1_event = edge_swarm_1.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { .. } = edge_swarm_1_event {
                    panic!("Connection with edge swarm 1 should not be closed for the duration of the test.");
                }
            }
            edge_swarm_2_event = edge_swarm_2.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = edge_swarm_2_event {
                    assert_eq!(peer_id, *blend_swarm.local_peer_id());
                    assert!(endpoint.is_dialer());
                    edge_swarm_connection_2_closed = true;
                }
            }
            blend_swarm_event = blend_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = blend_swarm_event {
                    assert_eq!(peer_id, *edge_swarm_2.local_peer_id());
                    assert!(endpoint.is_listener());
                }
            }
        }
    }

    assert!(edge_swarm_connection_2_closed);
}

#[test(tokio::test)]
async fn incoming_connection_network_too_small() {
    let mut edge_swarm = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut core_swarm = TestSwarm::new_ephemeral(|_| {
        // Add random peer to membership so the swarm is considered edge node.
        BehaviourBuilder::new(PeerId::random())
            // One remote and one local node. So `3` is the minimum value that is larger than the
            // used membership.
            .with_minimum_network_size(3)
            .build()
    });

    core_swarm.listen().with_memory_addr_external().await;

    // Then we perform the dial.
    edge_swarm.connect(&mut core_swarm).await;

    loop {
        select! {
            _ = edge_swarm.select_next_some() => {}
            core_swarm_event = core_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = core_swarm_event {
                    assert_eq!(peer_id, *edge_swarm.local_peer_id());
                    assert!(endpoint.is_listener());
                    break;
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn concurrent_incoming_connection_and_maximum_peering_degree_reached() {
    let mut listening_swarm = TestSwarm::new_ephemeral(|_| {
        BehaviourBuilder::new(PeerId::random())
            .with_max_incoming_connections(1)
            .with_timeout(Duration::from_secs(13))
            .build()
    });
    let listening_swarm_peer_id = *listening_swarm.local_peer_id();
    let mut dialer_swarm_1 = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut dialer_swarm_2 = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());

    let (listening_address, _) = listening_swarm.listen().await;

    dialer_swarm_1.dial(listening_address.clone()).unwrap();
    dialer_swarm_2.dial(listening_address.clone()).unwrap();

    let mut dialer_swarm_1_dropped = false;
    let mut dialer_swarm_2_dropped = false;
    // We need to hold a reference to the stream so that the estbalished connection
    // is not dropped pro-actively and we can test this logic.
    let s1: Arc<Mutex<Option<Stream>>> = Arc::new(Mutex::new(None));
    let s2: Arc<Mutex<Option<Stream>>> = Arc::new(Mutex::new(None));
    loop {
        select! {
            () = sleep(Duration::from_secs(11)) => {
                break;
            }
            _ = listening_swarm.select_next_some() => {}
            dialer_swarm_1_event = dialer_swarm_1.select_next_some() => {
                match dialer_swarm_1_event {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        assert_eq!(peer_id, listening_swarm_peer_id);
                        let mut control = dialer_swarm_1.behaviour_mut().new_control();
                        let stream_ref = Arc::clone(&s1);
                        // By the time this is called, the listening swarm might not be ready, so calling `await` and blocking this thread will result in a timeout. Hence we need to spawn a different task and store the stream into its mutex.
                        spawn(async move { *stream_ref.lock().unwrap() = Some(control.open_stream(listening_swarm_peer_id, PROTOCOL_NAME).await.unwrap()); });
                    }
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, listening_swarm_peer_id);
                        assert!(endpoint.is_dialer());
                        assert!(!dialer_swarm_2_dropped);
                        dialer_swarm_1_dropped = true;
                    }
                    _ => {}
                }
            }
            dialer_swarm_2_event = dialer_swarm_2.select_next_some() => {
                match dialer_swarm_2_event {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        assert_eq!(peer_id, listening_swarm_peer_id);
                        let mut control = dialer_swarm_2.behaviour_mut().new_control();
                        let stream_ref = Arc::clone(&s2);
                        spawn(async move { *stream_ref.lock().unwrap() = Some(control.open_stream(listening_swarm_peer_id, PROTOCOL_NAME).await.unwrap()); });
                    }
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, listening_swarm_peer_id);
                        assert!(endpoint.is_dialer());
                        assert!(!dialer_swarm_1_dropped);
                        dialer_swarm_2_dropped = true;
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(dialer_swarm_1_dropped ^ dialer_swarm_2_dropped);
}

#[test(tokio::test)]
async fn outgoing_connection_to_edge_peer() {
    let mut core_swarm =
        TestSwarm::new_ephemeral(|_| BehaviourBuilder::new(PeerId::random()).build());
    let mut edge_swarm = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());

    edge_swarm.listen().with_memory_addr_external().await;
    core_swarm.connect(&mut edge_swarm).await;

    // We use a dummy connection handler for core->edge outgoing connections, so
    // upgrading the substream should not be allowed.
    let stream_control_res = edge_swarm
        .behaviour_mut()
        .new_control()
        .open_stream(*core_swarm.local_peer_id(), PROTOCOL_NAME)
        .await;
    assert!(matches!(
        stream_control_res,
        Err(OpenStreamError::UnsupportedProtocol(_))
    ));
}
