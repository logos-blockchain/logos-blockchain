use core::time::Duration;

use futures::StreamExt as _;
use lb_blend_scheduling::membership::{Membership, Node};
use lb_key_management_system_keys::keys::{ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey};
use lb_libp2p::SwarmEvent;
use libp2p::{Multiaddr, PeerId};
use libp2p_stream::Behaviour as StreamBehaviour;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::TestSwarm,
    with_edge::behaviour::tests::utils::{BehaviourBuilder, StreamBehaviourExt as _},
};

/// After `start_new_session()`, the edge behaviour must immediately close all
/// existing upgraded edge peer connections (no dual-session for edge).
#[test(tokio::test)]
async fn start_new_session_closes_all_edge_connections() {
    let core_membership_peer = PeerId::random();
    let mut edge_swarm = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut blend_swarm = TestSwarm::new_ephemeral(|_| {
        BehaviourBuilder::new(core_membership_peer)
            .with_timeout(Duration::from_secs(30))
            .build()
    });

    blend_swarm.listen().with_memory_addr_external().await;

    // Establish and upgrade the edge connection.
    let _stream = edge_swarm
        .connect_and_upgrade_to_blend(&mut blend_swarm)
        .await;

    assert_eq!(blend_swarm.behaviour().upgraded_edge_peers.len(), 1);

    // Start a new session: all edge connections should be closed immediately.
    let new_membership = Membership::new_without_local(&[Node {
        address: Multiaddr::empty(),
        id: core_membership_peer,
        public_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
    }]);
    blend_swarm
        .behaviour_mut()
        .start_new_session(new_membership);

    assert_eq!(
        blend_swarm.behaviour().upgraded_edge_peers.len(),
        0,
        "All edge peers must be removed immediately after start_new_session"
    );

    // Drive the swarms until the connection actually closes.
    loop {
        select! {
            _ = edge_swarm.select_next_some() => {}
            event = blend_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, .. } = event {
                    assert_eq!(peer_id, *edge_swarm.local_peer_id());
                    break;
                }
            }
            () = sleep(Duration::from_secs(15)) => {
                panic!("Timed out waiting for edge connection to close after session transition");
            }
        }
    }
}

/// After a session transition with an updated membership, the edge behaviour
/// should accept new edge connections based on the new membership.
#[test(tokio::test)]
async fn session_transition_updates_membership_for_new_connections() {
    // Initially, edge_swarm_1's peer id is in the membership (core), so it
    // should be rejected. After session transition with a different membership
    // that does NOT contain edge_swarm_1, it becomes an edge node.
    let mut edge_swarm = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let edge_peer_id = *edge_swarm.local_peer_id();

    // Create blend_swarm with edge_peer_id in the membership (it's treated as
    // a core peer, so connections from it are denied).
    let mut blend_swarm = TestSwarm::new_ephemeral(|_| {
        BehaviourBuilder::new(edge_peer_id)
            .with_timeout(Duration::from_secs(30))
            .build()
    });

    blend_swarm.listen().with_memory_addr_external().await;

    // Edge swarm tries to connect. Since edge_peer_id is in the core membership,
    // the connection handler should be a dummy (no upgrade).
    edge_swarm.connect(&mut blend_swarm).await;

    // The connection should close since the edge node is actually in the core
    // membership.
    loop {
        select! {
            _ = edge_swarm.select_next_some() => {}
            event = blend_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, .. } = event {
                    assert_eq!(peer_id, edge_peer_id);
                    break;
                }
            }
            () = sleep(Duration::from_secs(15)) => {
                panic!("Timed out waiting for denied core peer connection to close");
            }
        }
    }

    // Transition to a new session where the membership no longer includes
    // edge_peer_id. Now edge_peer_id is a real edge node.
    let other_core_peer = PeerId::random();
    let new_membership = Membership::new_without_local(&[Node {
        address: Multiaddr::empty(),
        id: other_core_peer,
        public_key: Ed25519PublicKey::from_bytes(&[0; _]).unwrap(),
    }]);
    blend_swarm
        .behaviour_mut()
        .start_new_session(new_membership);

    // Now edge_swarm should be able to connect and upgrade.
    let _stream = edge_swarm
        .connect_and_upgrade_to_blend(&mut blend_swarm)
        .await;

    assert_eq!(
        blend_swarm.behaviour().upgraded_edge_peers.len(),
        1,
        "Edge peer should be accepted after session transition updated membership"
    );
}
