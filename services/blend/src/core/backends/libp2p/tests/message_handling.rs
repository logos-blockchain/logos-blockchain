use core::time::Duration;

use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{
    spawn,
    time::{sleep, timeout},
};

use crate::{
    core::backends::libp2p::{
        core_swarm_test_utils::new_nodes_with_empty_address,
        swarm::BlendSwarmMessage,
        tests::utils::{BlendBehaviourBuilder, SwarmBuilder, TestSwarm},
    },
    test_utils::TestEncapsulatedMessage,
};

#[test(tokio::test)]
async fn core_message_propagation() {
    let (mut identities, peer_ids) = new_nodes_with_empty_address(3);
    let TestSwarm {
        swarm: mut swarm_1,
        swarm_message_sender: swarm_1_message_sender,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let TestSwarm {
        swarm: mut swarm_2, ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let TestSwarm {
        swarm: mut swarm_3,
        incoming_message_receiver: mut swarm_3_message_receiver,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

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
        .send(BlendSwarmMessage::Publish {
            message: Box::new(message.clone()),
            epoch: 1.into(),
        })
        .await
        .unwrap();

    // We test that swarm 1 publishes a message, sending it to swarm 2, the only
    // swarm it is connected to. Then swarm 2 verifies its public header and
    // forwards it to swarm 3, which is not connected to swarm 1.
    let (swarm_3_received_message, epoch) = swarm_3_message_receiver.recv().await.unwrap();
    assert_eq!(swarm_3_received_message, message.clone());
    assert_eq!(epoch, 1);
}

/// A message whose `PoQ` does not verify is neither relayed to the other core
/// nodes nor bubbled up to the service. The forwarding functions only accept an
/// `EncapsulatedMessageWithVerifiedPublicHeader`, so a node cannot amplify
/// traffic it was unable to verify.
#[test(tokio::test)]
async fn message_with_invalid_poq_is_not_relayed() {
    let (mut identities, peer_ids) = new_nodes_with_empty_address(3);
    let TestSwarm {
        swarm: mut swarm_1,
        swarm_message_sender: swarm_1_message_sender,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    // The middle swarm rejects every `PoQ` it is asked to verify.
    let TestSwarm {
        swarm: mut swarm_2,
        incoming_message_receiver: mut swarm_2_message_receiver,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .with_rejecting_proofs_verifier()
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());
    let TestSwarm {
        swarm: mut swarm_3,
        incoming_message_receiver: mut swarm_3_message_receiver,
        ..
    } = SwarmBuilder::new(identities.next().unwrap(), &peer_ids)
        .build(|id, membership| BlendBehaviourBuilder::new(id, membership).build());

    let (swarm_2_address, _) = swarm_2.listen().await;
    let (swarm_3_address, _) = swarm_3.listen().await;

    swarm_1.dial_peer_at_addr(*swarm_2.local_peer_id(), swarm_2_address);
    swarm_2.dial_peer_at_addr(*swarm_3.local_peer_id(), swarm_3_address);

    spawn(async move { swarm_1.run().await });
    spawn(async move { swarm_2.run().await });
    spawn(async move { swarm_3.run().await });

    // Wait for peers to establish connections with each other
    sleep(Duration::from_secs(1)).await;

    swarm_1_message_sender
        .send(BlendSwarmMessage::Publish {
            message: Box::new(TestEncapsulatedMessage::new(b"test-payload").clone()),
            epoch: 1.into(),
        })
        .await
        .unwrap();

    // Swarm 2 drops the message on failed verification, so it never reaches swarm
    // 3, which is only connected to swarm 2.
    assert!(
        timeout(Duration::from_secs(2), swarm_3_message_receiver.recv())
            .await
            .is_err(),
        "A message whose PoQ failed to verify must not be relayed"
    );
    // Nor is it handed to swarm 2's own service.
    assert!(
        swarm_2_message_receiver.try_recv().is_err(),
        "A message whose PoQ failed to verify must not be reported to the service"
    );
}
