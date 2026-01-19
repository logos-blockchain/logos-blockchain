use futures::StreamExt as _;
use lb_core::{da::BlobId, sdp::SessionNumber};
use lb_utils::net::get_available_udp_port;
use logos_blockchain_tests::{
    common::da::{disseminate_with_metadata, setup_test_channel, wait_for_blob_onchain},
    nodes::executor::Executor,
    topology::{Topology, TopologyConfig},
};
use rand::{Rng as _, thread_rng};
use serial_test::serial;

#[ignore = "TODO: Enable when SDP Declarations are processed"]
#[tokio::test]
#[serial]
async fn update_membership_and_disseminate() {
    let topology_config = TopologyConfig::validator_and_executor();
    let n_participants = topology_config.n_validators + topology_config.n_executors;

    let (ids, da_ports, blend_ports) = generate_test_ids_and_ports(n_participants);
    let topology =
        Topology::spawn_with_empty_membership(topology_config, &ids, &da_ports, &blend_ports).await;

    topology.wait_network_ready().await;
    topology
        .wait_membership_empty_for_session(SessionNumber::from(0u64))
        .await;

    // TODO: Create a new membership with DA nodes.
    topology
        .wait_membership_ready_for_session(SessionNumber::from(1u64))
        .await;

    perform_dissemination_tests(&topology.executors()[0]).await;
}

fn generate_test_ids_and_ports(n_participants: usize) -> (Vec<[u8; 32]>, Vec<u16>, Vec<u16>) {
    let mut ids = vec![[0; 32]; n_participants];
    let mut da_ports = vec![];
    let mut blend_ports = vec![];

    for id in &mut ids {
        thread_rng().fill(id);
        da_ports.push(get_available_udp_port().unwrap());
        blend_ports.push(get_available_udp_port().unwrap());
    }

    (ids, da_ports, blend_ports)
}

async fn perform_dissemination_tests(executor: &Executor) {
    const ITERATIONS: usize = 10;

    let (test_channel_id, mut parent_msg_id) = setup_test_channel(executor).await;

    let data = [1u8; 31];

    for i in 0..ITERATIONS {
        println!("iteration {i}");
        let blob_id = disseminate_with_metadata(executor, test_channel_id, parent_msg_id, &data)
            .await
            .unwrap();

        parent_msg_id = wait_for_blob_onchain(executor, test_channel_id, blob_id).await;

        verify_share_replication(executor, blob_id).await;
    }
}

async fn verify_share_replication(executor: &Executor, blob_id: BlobId) {
    let shares = executor
        .get_shares(blob_id, [].into(), [].into(), true)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert_eq!(shares.len(), 2);
}
