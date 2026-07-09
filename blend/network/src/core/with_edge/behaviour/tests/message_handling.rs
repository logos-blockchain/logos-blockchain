use core::time::Duration;

use futures::{StreamExt as _, select};
use lb_blend_scheduling::serialize_encapsulated_message_with_verified_public_header;
use lb_libp2p::SwarmEvent;
use libp2p::PeerId;
use libp2p_stream::Behaviour as StreamBehaviour;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;

use crate::{
    core::{
        tests::utils::{TestEncapsulatedMessage, TestSwarm},
        with_edge::behaviour::{
            Event,
            tests::utils::{BehaviourBuilder, StreamBehaviourExt as _},
        },
    },
    send_msg,
};

#[test(tokio::test)]
async fn receive_valid_message() {
    let mut core_swarm =
        TestSwarm::new_ephemeral(|_| BehaviourBuilder::new(PeerId::random()).build());
    let mut edge_swarm = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());

    core_swarm.listen().with_memory_addr_external().await;
    let stream = edge_swarm
        .connect_and_upgrade_to_blend(&mut core_swarm)
        .await;
    let message = TestEncapsulatedMessage::new(b"test");
    send_msg(
        stream,
        serialize_encapsulated_message_with_verified_public_header(message.as_ref()),
    )
    .await
    .unwrap();

    loop {
        select! {
            _ = edge_swarm.select_next_some() => {}
            core_swarm_event = core_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::Message(received_message)) = core_swarm_event {
                    assert_eq!(received_message, message.into_inner().into());
                    break;
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn reject_message_with_unexpected_layer_count() {
    // The behaviour is configured to expect a single encapsulation layer, but
    // the shared test helper builds a 3-layer message. The size gate in
    // `EncapsulatedMessage::deserialize_from_remote` must reject it up front,
    // before the (larger) message is fully parsed or its header processed.
    let mut core_swarm = TestSwarm::new_ephemeral(|_| {
        BehaviourBuilder::new(PeerId::random())
            .with_num_blend_layers(1)
            .build()
    });
    let mut edge_swarm = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());

    core_swarm.listen().with_memory_addr_external().await;
    let stream = edge_swarm
        .connect_and_upgrade_to_blend(&mut core_swarm)
        .await;
    let message = TestEncapsulatedMessage::new(b"unexpected_layer_count");
    send_msg(
        stream,
        serialize_encapsulated_message_with_verified_public_header(message.as_ref()),
    )
    .await
    .unwrap();

    loop {
        select! {
            _ = edge_swarm.select_next_some() => {}
            core_swarm_event = core_swarm.select_next_some() => {
                match core_swarm_event {
                    SwarmEvent::Behaviour(Event::Message(_)) => {
                        panic!("No `Message` event should be generated for a message with an unexpected number of layers.");
                    }
                    SwarmEvent::ConnectionClosed { peer_id, .. } => {
                        assert_eq!(peer_id, *edge_swarm.local_peer_id());
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

#[test(tokio::test)]
async fn message_timeout() {
    let mut core_swarm = TestSwarm::new_ephemeral(|_| {
        BehaviourBuilder::new(PeerId::random())
            .with_timeout(Duration::from_secs(1))
            .build()
    });
    let mut edge_swarm = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());

    core_swarm.listen().with_memory_addr_external().await;
    let _stream = edge_swarm
        .connect_and_upgrade_to_blend(&mut core_swarm)
        .await;

    // Do not send a message. Stream should be dropped when the timeout is
    // reached, and connection closed when the swarm decides it.

    loop {
        select! {
            edge_swarm_event = edge_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = edge_swarm_event {
                    assert_eq!(peer_id, *core_swarm.local_peer_id());
                    assert!(endpoint.is_dialer());
                    break;
                }
            }
            _ = core_swarm.select_next_some() => {}
        }
    }
}

#[test(tokio::test)]
async fn receive_malformed_message() {
    let mut core_swarm =
        TestSwarm::new_ephemeral(|_| BehaviourBuilder::new(PeerId::random()).build());
    let mut edge_swarm = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());

    core_swarm.listen().with_memory_addr_external().await;
    let stream = edge_swarm
        .connect_and_upgrade_to_blend(&mut core_swarm)
        .await;
    let malformed_message = TestEncapsulatedMessage::new_with_invalid_signature(b"invalid_message");
    send_msg(
        stream,
        serialize_encapsulated_message_with_verified_public_header(malformed_message.as_ref()),
    )
    .await
    .unwrap();

    loop {
        select! {
            _ = edge_swarm.select_next_some() => {}
            core_swarm_event = core_swarm.select_next_some() => {
                match core_swarm_event {
                    SwarmEvent::Behaviour(Event::Message(_)) => {
                        panic!("No `Message` event should be generated for an invalid message received.");
                    }
                    SwarmEvent::ConnectionClosed { peer_id, .. } => {
                        assert_eq!(peer_id, *edge_swarm.local_peer_id());
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}
