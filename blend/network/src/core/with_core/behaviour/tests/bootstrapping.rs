use core::time::Duration;

use futures::StreamExt as _;
use lb_libp2p::SwarmEvent;
use libp2p::{
    core::Endpoint,
    swarm::{ConnectionId, dummy},
};
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::TestSwarm,
    with_core::behaviour::{
        Event,
        tests::utils::{BehaviourBuilder, SwarmExt as _, new_nodes_with_empty_address},
    },
};

#[test(tokio::test)]
async fn dialing_peer_not_supporting_blend_protocol() {
    let mut blend_swarm = TestSwarm::new_ephemeral(|id| BehaviourBuilder::new(id).build());
    let mut dummy_swarm = TestSwarm::new_ephemeral(|_| dummy::Behaviour);

    blend_swarm.listen().with_memory_addr_external().await;
    dummy_swarm.connect(&mut blend_swarm).await;

    let mut events_to_match = 2u8;
    loop {
        select! {
            blend_event = blend_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = blend_event {
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
async fn listening_peer_not_supporting_blend_protocol() {
    let mut blend_swarm = TestSwarm::new_ephemeral(|id| BehaviourBuilder::new(id).build());
    let mut dummy_swarm = TestSwarm::new_ephemeral(|_| dummy::Behaviour);

    dummy_swarm.listen().with_memory_addr_external().await;
    blend_swarm.connect(&mut dummy_swarm).await;

    let mut events_to_match = 2u8;
    loop {
        select! {
            blend_event = blend_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = blend_event {
                    assert_eq!(peer_id, *dummy_swarm.local_peer_id());
                    assert!(endpoint.is_dialer());
                    events_to_match -= 1;
                }
            }
            dummy_event = dummy_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = dummy_event {
                    assert_eq!(peer_id, *blend_swarm.local_peer_id());
                    assert!(endpoint.is_listener());
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
async fn incoming_connection_network_too_small() {
    let mut listening_swarm = TestSwarm::new_ephemeral(|id| {
        BehaviourBuilder::new(id)
            // Minimum network size of 2 with one-node (local) membership.
            .with_minimum_network_size(2)
            .build()
    });
    let mut dialing_swarm = TestSwarm::new_ephemeral(|id| BehaviourBuilder::new(id).build());

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm.connect(&mut listening_swarm).await;

    loop {
        select! {
            _ = dialing_swarm.select_next_some() => {}
            listening_swarm_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = listening_swarm_event {
                    assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                    assert!(endpoint.is_listener());
                    break;
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn outgoing_connection_network_too_small() {
    let mut listening_swarm = TestSwarm::new_ephemeral(|id| BehaviourBuilder::new(id).build());
    let mut dialing_swarm = TestSwarm::new_ephemeral(|id| {
        BehaviourBuilder::new(id)
            // Minimum network size of 2 with one-node (local) membership.
            .with_minimum_network_size(2)
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm.connect(&mut listening_swarm).await;

    loop {
        select! {
            dialing_swarm_event = dialing_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = dialing_swarm_event {
                    assert_eq!(peer_id, *listening_swarm.local_peer_id());
                    assert!(endpoint.is_dialer());
                    break;
                }
            }
            _ = listening_swarm.select_next_some() => {}
        }
    }
}

#[test(tokio::test)]
async fn incoming_attempt_with_max_negotiated_peering_degree() {
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut dialer_swarm_1 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut dialer_swarm_2 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listening_swarm.listen().with_memory_addr_external().await;

    dialer_swarm_1
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // We can call `connect` since a new connection will be established, but
    // will fail to upgrade (which we test below).
    dialer_swarm_2.connect(&mut listening_swarm).await;

    loop {
        select! {
            listening_swarm_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = listening_swarm_event {
                    assert_eq!(peer_id, *dialer_swarm_2.local_peer_id());
                    assert!(endpoint.is_listener());
                    break;
                }
            }
            _ = dialer_swarm_1.select_next_some() => {}
            _ = dialer_swarm_2.select_next_some() => {}
        }
    }
}

#[test(tokio::test)]
async fn concurrent_incoming_connections() {
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut dialer_swarm_1 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut dialer_swarm_2 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    let (listening_address, _) = listening_swarm.listen().await;

    // Dial concurrently before we poll the listening swarm.
    dialer_swarm_1.dial(listening_address.clone()).unwrap();
    dialer_swarm_2.dial(listening_address).unwrap();

    let mut dialer_1_dropped = false;
    let mut dialer_1_notified = false;
    let mut dialer_2_dropped = false;
    let mut dialer_2_notified = false;
    loop {
        select! {
            // We make sure that after 11 seconds one of the two connections is dropped (the swarm used in the tests uses a default timeout of 10s).
            // We cannot prevent both connections from being upgraded, but we can test that one of the two is dropped once the listener realized it is above the maximum peering degree.
            // We do not know which one beforehand because they are started in parallel.
            () = sleep(Duration::from_secs(11)) => {
                break;
            }
            listening_swarm_event = listening_swarm.select_next_some() => {
                // We check that the listening swarm never generates a `PeerDisconnected` event because it knows the dropped connection is meant to be ignored.
                if let SwarmEvent::Behaviour(Event::PeerDisconnected(_, _)) = listening_swarm_event {
                    panic!("Should not generate a `PeerDisconnected` event for a peer that went above our peering degree.");
                }
            }
            dialer_swarm_1_event = dialer_swarm_1.select_next_some() => {
                match dialer_swarm_1_event {
                    SwarmEvent::ConnectionClosed { endpoint, peer_id, .. } => {
                        assert!(!dialer_2_dropped);
                        assert_eq!(peer_id, *listening_swarm.local_peer_id());
                        assert!(endpoint.is_dialer());
                        dialer_1_dropped = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) if peer_id == *listening_swarm.local_peer_id() => {
                        dialer_1_notified = true;
                    }
                    _ => {}
                }
            }
            dialer_swarm_2_event = dialer_swarm_2.select_next_some() => {
                match dialer_swarm_2_event {
                    SwarmEvent::ConnectionClosed { endpoint, peer_id, .. } => {
                        assert!(!dialer_1_dropped);
                        assert_eq!(peer_id, *listening_swarm.local_peer_id());
                        assert!(endpoint.is_dialer());
                        dialer_2_dropped = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) if peer_id == *listening_swarm.local_peer_id() => {
                        dialer_2_notified = true;
                    }
                    _ => {}
                }
            }
        }
    }

    // We check whether the dialer whose connection was dropped was also notified by
    // its behaviour that the dialed peer got disconnected.
    assert!((dialer_1_dropped && dialer_1_notified) ^ (dialer_2_dropped && dialer_2_notified));
}

#[test(tokio::test)]
async fn incoming_attempt_with_duplicate_connection() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut dialer_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listening_swarm.listen().with_memory_addr_external().await;

    dialer_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;
    // This call will result in a closed connection.
    dialer_swarm.connect(&mut listening_swarm).await;

    loop {
        select! {
            () = sleep(Duration::from_secs(11)) => {
                break;
            }
            listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *dialer_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                    }
                    // Listener swarm should not know about this
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) => {
                        assert!(peer_id != *dialer_swarm.local_peer_id());
                    }
                    _ => {}
                }
            }
            // Neither should the dialer
            dialer_swarm_event = dialer_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) = dialer_swarm_event {
                    assert!(peer_id != *dialer_swarm.local_peer_id());
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn outgoing_attempt_with_max_negotiated_peering_degree() {
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm_1 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm_2 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listening_swarm_1.listen().with_memory_addr_external().await;
    listening_swarm_2.listen().with_memory_addr_external().await;

    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm_1)
        .await;

    // We can call `connect` since a new connection will be established, but
    // will fail to upgrade (which we test below).
    dialing_swarm.connect(&mut listening_swarm_2).await;

    loop {
        select! {
            dialer_swarm_event = dialing_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = dialer_swarm_event {
                    assert_eq!(peer_id, *listening_swarm_2.local_peer_id());
                    assert!(endpoint.is_dialer());
                    break;
                }
            }
            _ = listening_swarm_1.select_next_some() => {}
            _ = listening_swarm_2.select_next_some() => {}
        }
    }
}

#[test(tokio::test)]
async fn concurrent_outgoing_connections() {
    let (mut identities, nodes) = new_nodes_with_empty_address(3);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm_1 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm_2 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    let (listening_address_1, _) = listening_swarm_1.listen().await;
    let (listening_address_2, _) = listening_swarm_2.listen().await;

    dialing_swarm.dial(listening_address_1).unwrap();
    dialing_swarm.dial(listening_address_2).unwrap();

    let mut listener_1_dropped = false;
    let mut listener_1_notified = false;
    let mut listener_2_dropped = false;
    let mut listener_2_notified = false;
    loop {
        select! {
            // We make sure that after 11 seconds one of the two connections is dropped (the swarm used in the tests uses a default timeout of 10s).
            // We cannot prevent both connections from being upgraded, but we can test that one of the two is dropped once the dialer realized it is above the maximum peering degree.
            // We do not know which one beforehand because they are started in parallel.
            () = sleep(Duration::from_secs(11)) => {
                break;
            },
            dialing_swarm_event = dialing_swarm.select_next_some() => {
                // We check that the dialing swarm never generates a `PeerDisconnected` event because it knows the dropped connection is meant to be ignored.
                if let SwarmEvent::Behaviour(Event::PeerDisconnected(_, _)) = dialing_swarm_event {
                    panic!("Should not generate a `PeerDisconnected` event for a peer that went above our peering degree.");
                }
            }
            listener_swarm_1_event = listening_swarm_1.select_next_some() => {
                match listener_swarm_1_event {
                    SwarmEvent::ConnectionClosed { endpoint, peer_id, .. } => {
                        assert!(!listener_2_dropped);
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                        listener_1_dropped = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) if peer_id == *dialing_swarm.local_peer_id() => {
                        listener_1_notified = true;
                    }
                    _ => {}
                }
            }
            listener_swarm_2_event = listening_swarm_2.select_next_some() => {
                match listener_swarm_2_event {
                    SwarmEvent::ConnectionClosed { endpoint, peer_id, .. } => {
                        assert!(!listener_1_dropped);
                        assert_eq!(peer_id, *dialing_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                        listener_2_dropped = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) if peer_id == *dialing_swarm.local_peer_id() => {
                        listener_2_notified = true;
                    }
                    _ => {}
                }
            }
        }
    }

    // We check whether the listener whose connection was dropped was also notified
    // by its behaviour that the dialed peer got disconnected.
    assert!(
        (listener_1_dropped && listener_1_notified) || (listener_2_dropped && listener_2_notified)
    );
}

#[test(tokio::test)]
async fn outgoing_attempt_with_duplicate_connection() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialer_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    listening_swarm.listen().with_memory_addr_external().await;

    dialer_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;
    // This call will result in a closed connection.
    dialer_swarm.connect(&mut listening_swarm).await;

    loop {
        select! {
            () = sleep(Duration::from_secs(11)) => {
                break;
            }
             listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *dialer_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                    }
                    // Listener swarm should not know about this
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) => {
                        assert!(peer_id != *dialer_swarm.local_peer_id());
                    }
                    _ => {}
                }
            }
            // Neither should the dialer
            dialer_swarm_event = dialer_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) = dialer_swarm_event {
                    assert!(peer_id != *dialer_swarm.local_peer_id());
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn concurrent_same_direction_connections_between_peers() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut dialer_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });

    let (listening_address, _) = listening_swarm.listen().await;

    // Dial concurrently before we poll the listening swarm.
    dialer_swarm.dial(listening_address.clone()).unwrap();
    dialer_swarm.dial(listening_address).unwrap();

    let mut dropped_outgoing_connection_id: Option<ConnectionId> = None;
    let mut dropped_incoming_connection_id: Option<ConnectionId> = None;
    loop {
        select! {
            // We make sure that after 11 seconds one of the two connections is dropped (the swarm used in the tests uses a default timeout of 10s).
            // We cannot prevent both connections from being upgraded, but we can test that one of the two is dropped once the listener realized it is above the maximum peering degree.
            // We do not know which one beforehand because they are started in parallel.
            () = sleep(Duration::from_secs(11)) => {
                break;
            }
            listening_swarm_event = listening_swarm.select_next_some() => {
                match listening_swarm_event {
                    SwarmEvent::ConnectionClosed { endpoint, peer_id, connection_id, .. } => {
                        assert_eq!(peer_id, *dialer_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                        if dropped_incoming_connection_id.is_none() {
                            dropped_incoming_connection_id = Some(connection_id);
                        } else {
                            panic!("Only one connection should be closed.");
                        }
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) if peer_id == *dialer_swarm.local_peer_id() => {
                        panic!("No `PeerDisconnected` event should be generated for a listener which is dialed twice by the same peer.");
                    }
                    _ => {}
                }
            }
            dialing_swarm_event = dialer_swarm.select_next_some() => {
                match dialing_swarm_event {
                    SwarmEvent::ConnectionClosed { endpoint, peer_id, connection_id, .. } => {
                        assert_eq!(peer_id, *listening_swarm.local_peer_id());
                        assert!(endpoint.is_dialer());
                        if dropped_outgoing_connection_id.is_none() {
                            dropped_outgoing_connection_id = Some(connection_id);
                        } else {
                            panic!("Only one connection should be closed.");
                        }
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) if peer_id == *listening_swarm.local_peer_id() => {
                        panic!("No `PeerDisconnected` event should be generated for a dialer which dials twice the same peer.");
                    }
                    _ => {}
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn concurrent_reverse_connections_between_peers() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut swarm_1 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut swarm_2 = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let is_swarm_1_id_smaller = swarm_1.local_peer_id() <= swarm_2.local_peer_id();

    let (swarm_1_listening_address, _) = swarm_1.listen().await;
    let (swarm_2_listening_address, _) = swarm_2.listen().await;

    // Swarms dial each other concurrently.
    swarm_1.dial(swarm_2_listening_address).unwrap();
    swarm_2.dial(swarm_1_listening_address).unwrap();

    let mut swarm_1_outgoing_connection_id: Option<ConnectionId> = None;
    let mut swarm_1_incoming_connection_id: Option<ConnectionId> = None;
    let mut swarm_1_notified = false;
    let mut swarm_2_outgoing_connection_id: Option<ConnectionId> = None;
    let mut swarm_2_incoming_connection_id: Option<ConnectionId> = None;
    let mut swarm_2_notified = false;
    loop {
        select! {
            // We make sure that after 11 seconds one of the two connections is dropped (the swarm used in the tests uses a default timeout of 10s).
            () = sleep(Duration::from_secs(11)) => {
                break;
            }
            swarm_1_event = swarm_1.select_next_some() => {
                match swarm_1_event {
                    SwarmEvent::ConnectionEstablished { endpoint, peer_id, connection_id, .. } => {
                        assert_eq!(peer_id, *swarm_2.local_peer_id());
                        if endpoint.is_dialer() {
                            assert!(swarm_1_outgoing_connection_id.is_none());
                            swarm_1_outgoing_connection_id = Some(connection_id);
                        } else {
                            assert!(swarm_1_incoming_connection_id.is_none());
                            swarm_1_incoming_connection_id = Some(connection_id);
                        }
                    }
                    // We want to check that if swarm 1 ID is smaller, its outgoing connection will be closed, else its incoming one.
                    SwarmEvent::ConnectionClosed { endpoint, peer_id, connection_id, .. } => {
                        assert_eq!(peer_id, *swarm_2.local_peer_id());
                        assert!(!swarm_1_notified);
                        if is_swarm_1_id_smaller {
                            assert!(endpoint.is_dialer());
                            assert_eq!(Some(connection_id), swarm_1_outgoing_connection_id);
                        } else {
                            assert!(endpoint.is_listener());
                            assert_eq!(Some(connection_id), swarm_1_incoming_connection_id);
                        }
                        swarm_1_notified = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) if peer_id == *swarm_2.local_peer_id() => {
                        panic!("No `PeerDisconnected` event should be generated for duplicate connections to the same peer.");
                    }
                    _ => {}
                }
            }
            swarm_2_event = swarm_2.select_next_some() => {
                match swarm_2_event {
                    SwarmEvent::ConnectionEstablished { endpoint, peer_id, connection_id, .. } => {
                        assert_eq!(peer_id, *swarm_1.local_peer_id());
                        if endpoint.is_dialer() {
                            assert!(swarm_2_outgoing_connection_id.is_none());
                            swarm_2_outgoing_connection_id = Some(connection_id);
                        } else {
                            assert!(swarm_2_incoming_connection_id.is_none());
                            swarm_2_incoming_connection_id = Some(connection_id);
                        }
                    }
                    // We want to check that if swarm 2 ID is smaller, its outgoing connection will be closed, else its incoming one.
                    SwarmEvent::ConnectionClosed { endpoint, peer_id, connection_id, .. } => {
                        assert_eq!(peer_id, *swarm_1.local_peer_id());
                        assert!(!swarm_2_notified);
                        if is_swarm_1_id_smaller {
                            assert!(endpoint.is_listener());
                            assert_eq!(Some(connection_id), swarm_2_incoming_connection_id);
                        } else {
                            assert!(endpoint.is_dialer());
                            assert_eq!(Some(connection_id), swarm_2_outgoing_connection_id);
                        }
                        swarm_2_notified = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(peer_id, _)) if peer_id == *swarm_2.local_peer_id() => {
                        panic!("No `PeerDisconnected` event should be generated for duplicate connections to the same peer.");
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(swarm_1_notified && swarm_2_notified);

    let swarm_2_details_for_swarm_1 = swarm_1
        .behaviour()
        .negotiated_peers
        .get(swarm_2.local_peer_id())
        .unwrap();
    // If swarm 1 ID is lower, it must have closed its outgoing connection, so swarm
    // 2 will be the dialer, or viceversa.
    assert!(
        swarm_2_details_for_swarm_1.role
            == if is_swarm_1_id_smaller {
                Endpoint::Dialer
            } else {
                Endpoint::Listener
            }
    );
    let swarm_1_details_for_swarm_2 = swarm_2
        .behaviour()
        .negotiated_peers
        .get(swarm_1.local_peer_id())
        .unwrap();
    assert!(
        swarm_1_details_for_swarm_2.role
            == if is_swarm_1_id_smaller {
                Endpoint::Listener
            } else {
                Endpoint::Dialer
            }
    );
}

#[test(tokio::test)]
async fn replace_existing_with_new_connection() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut smaller_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(1..=2)
            .build()
    });
    let mut larger_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(1..=2)
            .build()
    });

    smaller_swarm.listen().with_memory_addr_external().await;
    larger_swarm.listen().with_memory_addr_external().await;

    smaller_swarm
        .connect_and_wait_for_upgrade(&mut larger_swarm)
        .await;

    larger_swarm.connect(&mut smaller_swarm).await;

    // We wait that the old connection is dropped and that the operation is
    // transparent to the swarm (i.e., the swarm is not notified about any of
    // this since it's internal logic).

    let mut smaller_swarm_connection_dropped = false;
    let mut larger_swarm_connection_dropped = false;
    let mut larger_swarm_connection_upgrade_notified = false;
    let mut smaller_swarm_connection_upgrade_notified = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(12)) => {
                break;
            }
            smaller_swarm_event = smaller_swarm.select_next_some() => {
                match smaller_swarm_event {
                    // Peer with lower ID closes its outgoing connection in favor of a new incoming.
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *larger_swarm.local_peer_id());
                        assert!(endpoint.is_dialer());
                        assert!(!smaller_swarm_connection_dropped);
                        smaller_swarm_connection_dropped = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(_, _)) => {
                        panic!("No `PeerDisconnected` event should be generated when an outgoing connection is replaced with an incoming one.");
                    }
                    SwarmEvent::Behaviour(Event::InboundConnectionUpgradeSucceeded(peer_id)) => {
                        assert_eq!(peer_id, *larger_swarm.local_peer_id());
                        assert!(!smaller_swarm_connection_upgrade_notified);
                        smaller_swarm_connection_upgrade_notified = true;
                    }
                    _ => {}
                }
            }
            larger_swarm_event = larger_swarm.select_next_some() => {
                match larger_swarm_event {
                    // Peer with larger ID closes its incoming connection in favor of a new outgoing.
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *smaller_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                        assert!(!larger_swarm_connection_dropped);
                        larger_swarm_connection_dropped = true;
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(_, _)) => {
                        panic!("No `PeerDisconnected` event should be generated when an incoming connection is replaced with an outgoing one.");
                    }
                    SwarmEvent::Behaviour(Event::OutboundConnectionUpgradeSucceeded(peer_id)) => {
                        assert_eq!(peer_id, *smaller_swarm.local_peer_id());
                        assert!(!larger_swarm_connection_upgrade_notified);
                        larger_swarm_connection_upgrade_notified = true;
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(
        smaller_swarm_connection_dropped
            && larger_swarm_connection_dropped
            && larger_swarm_connection_upgrade_notified
    );

    let larger_swarm_details_for_smaller_swarm = *smaller_swarm
        .behaviour()
        .negotiated_peers()
        .get(larger_swarm.local_peer_id())
        .unwrap();
    let smaller_swarm_details_for_larger_swarm = *larger_swarm
        .behaviour()
        .negotiated_peers()
        .get(smaller_swarm.local_peer_id())
        .unwrap();

    assert_eq!(
        larger_swarm_details_for_smaller_swarm.role,
        Endpoint::Dialer
    );
    assert_eq!(
        smaller_swarm_details_for_larger_swarm.role,
        Endpoint::Listener
    );
}

#[test(tokio::test)]
async fn discard_new_for_existing_connection() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut smaller_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(1..=2)
            .build()
    });
    let mut larger_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_peering_degree(1..=2)
            .build()
    });

    smaller_swarm.listen().with_memory_addr_external().await;
    larger_swarm.listen().with_memory_addr_external().await;

    larger_swarm
        .connect_and_wait_for_upgrade(&mut smaller_swarm)
        .await;

    smaller_swarm.connect(&mut larger_swarm).await;

    // We wait that the old connection is ignored and that the operation is
    // transparent to the swarm (i.e., the swarm is not notified about any of
    // this since it's internal logic).

    let mut smaller_swarm_connection_dropped = false;
    let mut larger_swarm_connection_dropped = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(12)) => {
                break;
            }
            smaller_swarm_event = smaller_swarm.select_next_some() => {
                match smaller_swarm_event {
                    // Peer with lower ID ignores the new outgoing connection in favor of the established incoming.
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *larger_swarm.local_peer_id());
                        assert!(endpoint.is_dialer());
                        assert!(!smaller_swarm_connection_dropped);
                        smaller_swarm_connection_dropped = true;
                    }
                    SwarmEvent::Behaviour(Event::OutboundConnectionUpgradeSucceeded(_)) => {
                        panic!("No new `OutboundConnectionUpgradeSucceeded` event should be generated when an outgoing connection is ignored for an existing incoming one.");
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(_, _)) => {
                        panic!("No `PeerDisconnected` event should be generated when an outgoing connection is ignored for an existing incoming one.");
                    }
                    _ => {}
                }
            }
            larger_swarm_event = larger_swarm.select_next_some() => {
                match larger_swarm_event {
                    // Peer with larger ID ignores the new incoming connection in favor of the established outgoing.
                    SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, *smaller_swarm.local_peer_id());
                        assert!(endpoint.is_listener());
                        assert!(!larger_swarm_connection_dropped);
                        larger_swarm_connection_dropped = true;
                    }
                    SwarmEvent::Behaviour(Event::InboundConnectionUpgradeSucceeded(_)) => {
                        panic!("No new `InboundConnectionUpgradeSucceeded` event should be generated when an incoming connection is ignored for an existing outgoing one.");
                    }
                    SwarmEvent::Behaviour(Event::PeerDisconnected(_, _)) => {
                        panic!("No `PeerDisconnected` event should be generated when an incoming connection is ignored for an existing outgoing one.");
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(smaller_swarm_connection_dropped && larger_swarm_connection_dropped);

    let larger_swarm_details_for_smaller_swarm = *smaller_swarm
        .behaviour()
        .negotiated_peers()
        .get(larger_swarm.local_peer_id())
        .unwrap();
    let smaller_swarm_details_for_larger_swarm = *larger_swarm
        .behaviour()
        .negotiated_peers()
        .get(smaller_swarm.local_peer_id())
        .unwrap();

    // Larger swarm maintains its outgoing connection.
    assert_eq!(
        larger_swarm_details_for_smaller_swarm.role,
        Endpoint::Dialer
    );
    // Smaller swarm maintains its incoming connection.
    assert_eq!(
        smaller_swarm_details_for_larger_swarm.role,
        Endpoint::Listener
    );
}
