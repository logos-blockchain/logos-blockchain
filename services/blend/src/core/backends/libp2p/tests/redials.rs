use core::time::Duration;

use lb_libp2p::{Protocol, SwarmEvent};
use libp2p::{Multiaddr, PeerId};
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::backends::libp2p::{
    core_swarm_test_utils::{SwarmExt as _, new_nodes_with_empty_address, update_nodes},
    tests::utils::{BlendBehaviourBuilder, SwarmBuilder, TestSwarm},
};

#[test(tokio::test)]
async fn core_redial_same_peer() {
    let (mut identities, peer_ids) = new_nodes_with_empty_address(1);
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let random_peer_id = PeerId::random();
    let empty_multiaddr: Multiaddr = Protocol::Memory(0).into();
    dialing_swarm.dial_peer_at_addr(random_peer_id, empty_multiaddr.clone());

    let dial_attempt_1_record = dialing_swarm.ongoing_dials().get(&random_peer_id).unwrap();
    assert_eq!(*dial_attempt_1_record.address(), empty_multiaddr.clone());
    assert_eq!(
        dial_attempt_1_record.attempt_number(),
        1.try_into().unwrap()
    );

    // We poll the swarm until we know the first dial attempt has failed.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    let dial_attempt_2_record = dialing_swarm.ongoing_dials().get(&random_peer_id).unwrap();
    assert_eq!(*dial_attempt_2_record.address(), empty_multiaddr.clone());
    assert_eq!(
        dial_attempt_2_record.attempt_number(),
        2.try_into().unwrap()
    );

    // We poll the swarm until the next failure.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    let dial_attempt_3_record = dialing_swarm.ongoing_dials().get(&random_peer_id).unwrap();
    assert_eq!(*dial_attempt_3_record.address(), empty_multiaddr.clone());
    assert_eq!(
        dial_attempt_3_record.attempt_number(),
        3.try_into().unwrap()
    );

    // We poll the swarm until the next failure, after which there should be no more
    // attempts.
    dialing_swarm
        .poll_next_until(|event| {
            let SwarmEvent::OutgoingConnectionError { peer_id, .. } = event else {
                return false;
            };
            *peer_id == Some(random_peer_id)
        })
        .await;

    // Storage map should be cleared up, and since there is no other peer, there is
    // no new peer that is dialed.
    assert!(dialing_swarm.ongoing_dials().is_empty());
}

#[test(tokio::test)]
async fn core_redial_different_peer_after_redial_limit() {
    let (mut identities, mut nodes) = new_nodes_with_empty_address(2);
    let TestSwarm {
        swarm: mut listening_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let (listening_node, _) = listening_swarm
        .listen_and_return_membership_entry(None)
        .await;
    update_nodes(&mut nodes, &listening_node.id, listening_node.address);

    // Build dialing swarm with the listening info of the listening swarm.
    let TestSwarm {
        swarm: mut dialing_swarm,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &nodes)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let dialing_peer_id = *dialing_swarm.local_peer_id();

    // Dial a random peer on a random address, which should fail after the maximum
    // number of attempts, after which the dialing swarm should connect to the
    // listening swarm.
    dialing_swarm.dial_peer_at_addr(PeerId::random(), Protocol::Memory(0).into());

    loop {
        select! {
            () = sleep(Duration::from_secs(3)) => {
                break;
            }
            () = dialing_swarm.poll_next() => {}
            () = listening_swarm.poll_next() => {}
        }
    }

    assert!(dialing_swarm.ongoing_dials().is_empty());
    assert!(
        dialing_swarm
            .behaviour()
            .blend
            .with_core()
            .negotiated_peers()
            .contains_key(&listening_node.id)
    );
    assert_eq!(
        dialing_swarm
            .behaviour()
            .blend
            .with_core()
            .num_healthy_peers(),
        1
    );
    assert!(
        listening_swarm
            .behaviour()
            .blend
            .with_core()
            .negotiated_peers()
            .contains_key(&dialing_peer_id)
    );
}
