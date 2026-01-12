use std::time::Duration;

use chain_service::StartingState;
use futures::StreamExt as _;
use kzgrs_backend::{common::share::DaShare, reconstruction::reconstruct_without_missing_data};
use serial_test::serial;
use subnetworks_assignations::MembershipHandler as _;
use tests::{
    common::da::{
        disseminate_with_metadata, setup_test_channel, wait_for_blob_onchain,
        wait_for_shares_number,
    },
    nodes::executor::{Executor, create_executor_config},
    secret_key_to_peer_id,
    topology::{Topology, TopologyConfig, configs::create_general_configs},
};
use tokio::time::interval;

#[ignore = "for manual usage, disseminate_retrieve_reconstruct is preferred for ci"]
#[tokio::test]
#[serial]
async fn disseminate_and_retrieve() {
    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;
    let executor = &topology.executors()[0];
    let (channel_id, parent_msg_id) = setup_test_channel(executor).await;
    let validator = &topology.validators()[0];

    tokio::time::sleep(Duration::from_secs(15)).await;

    let data = [1u8; 31];
    tokio::time::sleep(Duration::from_secs(15)).await;

    let blob_id = disseminate_with_metadata(executor, channel_id, parent_msg_id, &data)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(20)).await;

    let _ = wait_for_blob_onchain(executor, channel_id, blob_id).await;

    let executor_shares = executor
        .get_shares(blob_id, [].into(), [].into(), true)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    let validator_shares = validator
        .get_shares(blob_id, [].into(), [].into(), true)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    // Index zero shouldn't be empty, validator replicated both blobs to
    // executor because they both are in the same subnetwork.
    assert!(executor_shares.len() == 2);
    assert!(validator_shares.len() == 2);
}

#[tokio::test]
#[serial]
async fn disseminate_retrieve_reconstruct() {
    const ITERATIONS: usize = 10;

    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;

    topology.wait_membership_ready().await;
    topology.wait_network_ready().await;
    topology.wait_da_network_ready().await;

    let executor = &topology.executors()[0];
    let (channel_id, mut parent_msg_id) = setup_test_channel(executor).await;
    let num_samples = executor.config().da_sampling.sampling_settings.num_samples as usize;
    let data = [1u8; 31 * ITERATIONS];

    for i in 0..ITERATIONS {
        let data_size = 31 * (i + 1);
        println!("disseminating {data_size} bytes, iteration {i}");
        let data = &data[..data_size]; // test increasing size data
        let blob_id = disseminate_with_metadata(executor, channel_id, parent_msg_id, data)
            .await
            .unwrap();

        parent_msg_id = wait_for_blob_onchain(executor, channel_id, blob_id).await;

        wait_for_shares_number(executor, blob_id, num_samples).await;

        let share_commitments = executor.get_commitments(blob_id, 0).await.unwrap();
        let mut executor_shares = executor
            .get_shares(blob_id, [].into(), [].into(), true)
            .await
            .unwrap()
            .map(|light_share| DaShare::from((light_share, share_commitments.clone())))
            .collect::<Vec<_>>()
            .await;

        executor_shares.sort_by_key(|share| share.share_idx);

        // Reconstruction is performed from the one of the two shares.
        let reconstructed = reconstruct_without_missing_data(&[executor_shares[0].clone()]);

        assert_eq!(reconstructed, data);
    }

    let validator = &topology.validators()[0];

    // TODO think about a test with malicious/unhealthy peers that'd trigger
    // recording some monitor stats too
    assert_eq!(executor.balancer_stats().await.len(), 2);
    assert!(executor.monitor_stats().await.0.is_empty());
    assert_eq!(validator.balancer_stats().await.len(), 2);
    assert!(validator.monitor_stats().await.0.is_empty());
}

#[tokio::test]
#[serial]
async fn disseminate_from_non_membership() {
    const ITERATIONS: usize = 10;

    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;
    topology.wait_membership_ready().await;
    topology.wait_network_ready().await;
    topology.wait_da_network_ready().await;

    let membership_executor = &topology.executors()[0];
    let (channel_id, mut parent_msg_id) = setup_test_channel(membership_executor).await;
    let num_samples = membership_executor
        .config()
        .da_sampling
        .sampling_settings
        .num_samples as usize;

    let StartingState::Genesis { genesis_tx } = membership_executor
        .config()
        .cryptarchia
        .service
        .starting_state
        .clone()
    else {
        panic!("Non member executor expects genesis_tx as starting state");
    };

    let mut lone_general_config = create_general_configs(1).into_iter().next().unwrap();
    lone_general_config
        .consensus_config
        .override_genesis_tx(genesis_tx);
    let lone_executor_config = create_executor_config(lone_general_config);
    let lone_executor = Executor::spawn(lone_executor_config).await;

    // Wait for lone_executor to have membership info for session 0
    // (even though it's not in the membership, it needs to know about the session)
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if lone_executor
                .da_get_membership(nomos_core::sdp::SessionNumber::from(0u64))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("Timed out waiting for lone_executor membership");

    let data = [1u8; 31 * ITERATIONS];

    for i in 0..ITERATIONS {
        let data_size = 31 * (i + 1);
        println!("disseminating {data_size} bytes, iteration {i}");
        let data = &data[..data_size]; // test increasing size data
        let blob_id = disseminate_with_metadata(&lone_executor, channel_id, parent_msg_id, data)
            .await
            .unwrap();

        parent_msg_id = wait_for_blob_onchain(membership_executor, channel_id, blob_id).await;

        wait_for_shares_number(membership_executor, blob_id, num_samples).await;

        let share_commitments = membership_executor
            .get_commitments(blob_id, 0)
            .await
            .unwrap();
        let mut executor_shares = membership_executor
            .get_shares(blob_id, [].into(), [].into(), true)
            .await
            .unwrap()
            .map(|light_share| DaShare::from((light_share, share_commitments.clone())))
            .collect::<Vec<_>>()
            .await;

        executor_shares.sort_by_key(|share| share.share_idx);

        // Reconstruction is performed from the one of the two shares.
        let reconstructed = reconstruct_without_missing_data(&[executor_shares[0].clone()]);

        assert_eq!(reconstructed, data);
    }
}

#[ignore = "Reenable when tools to inspect mempool are added"]
#[tokio::test]
#[serial]
async fn four_subnets_disseminate_retrieve_reconstruct() {
    const ITERATIONS: usize = 10;

    let topology = Topology::spawn(TopologyConfig::validators_and_executor(3, 4, 2)).await;
    let membership = &topology.validators()[0].config().da_network.membership;

    let validator_subnet_0 = topology
        .validators()
        .iter()
        .find(|v| {
            let node_key = v.config().da_network.backend.node_key.clone();
            let peer_id = secret_key_to_peer_id(node_key);
            let subnets = membership.membership(&peer_id);
            subnets.contains(&0)
        })
        .expect("Validator subnet 0 not found");

    let validator_subnet_1 = topology
        .validators()
        .iter()
        .find(|v| {
            let node_key = v.config().da_network.backend.node_key.clone();
            let peer_id = secret_key_to_peer_id(node_key);
            let subnets = membership.membership(&peer_id);
            subnets.contains(&1)
        })
        .expect("Validator subnet 1 not found");

    let executor = &topology.executors()[0];

    let (test_channel_id, mut parent_msg_id) = setup_test_channel(executor).await;

    let data = [1u8; 31 * ITERATIONS];

    for i in 0..ITERATIONS {
        let data_size = 31 * (i + 1);
        println!("disseminating {data_size} bytes");
        let data = &data[..data_size]; // test increasing size data
        let blob_id = disseminate_with_metadata(executor, test_channel_id, parent_msg_id, data)
            .await
            .unwrap();

        parent_msg_id = wait_for_blob_onchain(executor, test_channel_id, blob_id).await;

        let share_commitments = validator_subnet_1
            .get_commitments(blob_id, 0)
            .await
            .unwrap();

        let mut validator_subnet_0_shares = validator_subnet_0
            .get_shares(blob_id, [].into(), [].into(), true)
            .await
            .unwrap()
            .map(|light_share| DaShare::from((light_share, share_commitments.clone())))
            .collect::<Vec<_>>()
            .await;
        validator_subnet_0_shares.sort_by_key(|share| share.share_idx);

        let mut validator_subnet_1_shares = validator_subnet_1
            .get_shares(blob_id, [].into(), [].into(), true)
            .await
            .unwrap()
            .map(|light_share| DaShare::from((light_share, share_commitments.clone())))
            .collect::<Vec<_>>()
            .await;
        validator_subnet_1_shares.sort_by_key(|share| share.share_idx);

        let reconstruction_shares = vec![
            validator_subnet_0_shares[0].clone(),
            validator_subnet_1_shares[0].clone(),
        ];

        // Reconstruction is performed from the one of the two blobs.
        let reconstructed = reconstruct_without_missing_data(&reconstruction_shares);
        assert_eq!(&reconstructed[..data.len()], data);
    }

    // TODO think about a test with malicious/unhealthy peers that'd trigger
    // recording some monitor stats too
    assert_eq!(executor.balancer_stats().await.len(), 2);
    assert!(executor.monitor_stats().await.0.is_empty());
    assert_eq!(validator_subnet_0.balancer_stats().await.len(), 2);
    assert!(validator_subnet_0.monitor_stats().await.0.is_empty());
}

#[tokio::test]
#[serial]
async fn disseminate_same_data() {
    const ITERATIONS: usize = 10;

    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;
    topology.wait_membership_ready().await;
    topology.wait_network_ready().await;
    topology.wait_da_network_ready().await;

    let executor = &topology.executors()[0];
    let num_subnets = executor.config().da_network.backend.num_subnets as usize;

    let (test_channel_id, mut parent_msg_id) = setup_test_channel(executor).await;

    let data = [1u8; 31];

    for _ in 0..ITERATIONS {
        let blob_id = disseminate_with_metadata(executor, test_channel_id, parent_msg_id, &data)
            .await
            .unwrap();

        parent_msg_id = wait_for_blob_onchain(executor, test_channel_id, blob_id).await;

        wait_for_shares_number(executor, blob_id, num_subnets).await;
        let executor_shares = executor
            .get_shares(blob_id, [].into(), [].into(), true)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        // Index zero shouldn't be empty, validator replicated both blobs to
        // executor because they both are in the same subnetwork.
        assert!(executor_shares.len() == 2);
    }
}

#[ignore = "for local debugging"]
#[tokio::test]
#[serial]
async fn local_testnet_one_node() {
    let topology = Topology::spawn(TopologyConfig::one_validator()).await;
    let validator = &topology.validators()[0];
    let addr = validator.config().http.backend_settings.address;
    println!("Validator http addr {addr:?}, example test url: http://{addr:?}/cryptarchia/info");

    let mut interval = interval(Duration::from_secs(10));
    loop {
        interval.tick().await;

        let info = validator.consensus_info().await;
        println!("Consensus info: {info:?}");
    }
}

#[ignore = "for local debugging"]
#[tokio::test]
#[serial]
async fn local_testnet() {
    let topology = Topology::spawn(TopologyConfig::validators_and_executor(3, 2, 2)).await;
    let executor = &topology.executors()[0];

    let (channel_id, mut parent_msg_id) = setup_test_channel(executor).await;
    let mut index = 0u64;
    loop {
        let blob_id =
            disseminate_with_metadata(executor, channel_id, parent_msg_id, &generate_data(index))
                .await
                .unwrap();
        parent_msg_id = wait_for_blob_onchain(executor, channel_id, blob_id).await;

        index += 1;
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

fn generate_data(index: u64) -> Vec<u8> {
    (index as u8..index as u8 + 31).collect()
}

#[ignore = "for local debugging"]
#[tokio::test]
#[serial]
async fn split_2025_death_payload() {
    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;
    let executor = &topology.executors()[0];

    let (channel_id, parent_msg_id) = setup_test_channel(executor).await;
    let data = vec![
        32, 0, 0, 0, 0, 0, 0, 0, 34, 88, 212, 64, 57, 70, 21, 63, 42, 117, 231, 187, 244, 0, 62,
        221, 185, 0, 148, 28, 70, 179, 1, 201, 225, 20, 77, 79, 243, 241, 218, 162, 32, 0, 0, 0, 0,
        0, 0, 0, 29, 204, 77, 232, 222, 199, 93, 122, 171, 133, 181, 103, 182, 204, 212, 26, 211,
        18, 69, 27, 148, 138, 116, 19, 240, 161, 66, 253, 64, 212, 147, 71, 20, 0, 0, 0, 0, 0, 0,
        0, 29, 209, 103, 120, 139, 92, 59, 145, 9, 212, 96, 137, 212, 35, 41, 169, 223, 154, 135,
        82, 32, 0, 0, 0, 0, 0, 0, 0, 101, 115, 93, 193, 193, 43, 221, 14, 39, 107, 24, 192, 210,
        179, 121, 113, 188, 187, 128, 225, 109, 16, 2, 49, 157, 189, 64, 190, 212, 223, 155, 24,
        32, 0, 0, 0, 0, 0, 0, 0, 86, 232, 31, 23, 27, 204, 85, 166, 255, 131, 69, 230, 146, 192,
        248, 110, 91, 72, 224, 27, 153, 108, 173, 192, 1, 98, 47, 181, 227, 99, 180, 33, 32, 0, 0,
        0, 0, 0, 0, 0, 86, 232, 31, 23, 27, 204, 85, 166, 255, 131, 69, 230, 146, 192, 248, 110,
        91, 72, 224, 27, 153, 108, 173, 192, 1, 98, 47, 181, 227, 99, 180, 33, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 141, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 37, 81, 0,
        8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 104, 7,
        69, 173, 0, 0, 0, 0, 0, 0, 0, 0, 32, 0, 0, 0, 0, 0, 0, 0, 52, 188, 157, 159, 212, 17, 50,
        190, 0, 212, 161, 18, 26, 74, 191, 11, 209, 193, 171, 234, 246, 67, 62, 223, 234, 162, 103,
        164, 102, 122, 168, 8, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 8, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 1, 32, 0, 0, 0, 0, 0, 0, 0, 86, 232, 31, 23, 27, 204, 85,
        166, 255, 131, 69, 230, 146, 192, 248, 110, 91, 72, 224, 27, 153, 108, 173, 192, 1, 98, 47,
        181, 227, 99, 180, 33, 1, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 8, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 32, 0, 0, 0, 0, 0, 0, 0, 62, 204, 108, 19, 224, 225,
        111, 123, 195, 72, 243, 83, 193, 82, 219, 39, 175, 7, 187, 46, 178, 197, 198, 153, 169, 60,
        128, 84, 169, 217, 162, 189, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let blob_id = disseminate_with_metadata(executor, channel_id, parent_msg_id, &data)
        .await
        .unwrap();
    let _ = wait_for_blob_onchain(executor, channel_id, blob_id).await;
}
