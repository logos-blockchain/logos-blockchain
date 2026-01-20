use core::time::Duration;

use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{spawn, time::sleep};

use crate::{
    core::backends::libp2p::{
        core_swarm_test_utils::new_nodes_with_empty_address,
        swarm::BlendSwarmMessage,
        tests::utils::{BlendBehaviourBuilder, SwarmBuilder, TestSwarm},
    },
    test_utils::{TestEncapsulatedMessage, crypto::MockProofsVerifier},
};

#[test(tokio::test)]
async fn core_message_propagation() {
    let (mut identities, peer_ids) = new_nodes_with_empty_address(3);
    let TestSwarm {
        swarm: mut swarm_1,
        swarm_message_sender: swarm_1_message_sender,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids).build(|id, membership| {
        BlendBehaviourBuilder::new(id, MockProofsVerifier, membership).build()
    });
    let TestSwarm {
        swarm: mut swarm_2, ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids).build(|id, membership| {
        BlendBehaviourBuilder::new(id, MockProofsVerifier, membership).build()
    });
    let TestSwarm {
        swarm: mut swarm_3,
        incoming_message_receiver: mut swarm_3_message_receiver,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids).build(|id, membership| {
        BlendBehaviourBuilder::new(id, MockProofsVerifier, membership).build()
    });

    let (swarm_2_address, _) = swarm_2.listen().await;
    let (swarm_3_address, _) = swarm_3.listen().await;

    swarm_1.dial_peer_at_addr(*swarm_2.local_peer_id(), swarm_2_address);
    swarm_2.dial_peer_at_addr(*swarm_3.local_peer_id(), swarm_3_address);

    spawn(async move { swarm_1.run().await });
    spawn(async move { swarm_2.run().await });
    spawn(async move { swarm_3.run().await });

    // Wait for peers to establish connections with each other
    sleep(Duration::from_secs(1)).await;

    let message = TestEncapsulatedMessage::new(b"test-payload");

    swarm_1_message_sender
        .send(BlendSwarmMessage::Publish(Box::new(message.clone().into())))
        .await
        .unwrap();

    // We test that swarm 1 publishes a message, sending it to swarm 2, the only
    // swarm it is connected to. Then swarm 2 forwards it to swarm 3, which is not
    // connected to swarm 1.
    let swarm_3_received_message = swarm_3_message_receiver.recv().await.unwrap();
    assert_eq!(swarm_3_received_message, message.into_inner());
}
