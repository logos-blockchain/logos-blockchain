use std::{collections::HashSet, time::Duration};

use common_http_client::CommonHttpClient;
use futures_util::stream::StreamExt as _;
use kzgrs_backend::common::share::DaShare;
use nomos_core::{da::blob::LightShare as _, sdp::SessionNumber};
use nomos_libp2p::ed25519;
use rand::{RngCore as _, rngs::OsRng};
use reqwest::Url;
use serial_test::serial;
use tests::{
    adjust_timeout,
    common::da::{
        DA_TESTS_TIMEOUT, disseminate_with_metadata, setup_test_channel, wait_for_blob_onchain,
    },
    nodes::validator::{Validator, create_validator_config},
    secret_key_to_peer_id,
    topology::{Topology, TopologyConfig, configs::create_general_configs},
};

#[tokio::test]
#[serial]
async fn test_get_share_data() {
    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;

    topology.wait_membership_ready().await;
    topology.wait_network_ready().await;
    topology.wait_da_network_ready().await;

    let executor = &topology.executors()[0];
    let (channel_id, parent_msg_id) = setup_test_channel(executor).await;

    let data = [1u8; 31];
    let blob_id = disseminate_with_metadata(executor, channel_id, parent_msg_id, &data)
        .await
        .unwrap();

    let _ = wait_for_blob_onchain(executor, channel_id, blob_id).await;

    // Wait for transactions to be stored
    tokio::time::sleep(Duration::from_secs(2)).await;

    let executor_shares = executor
        .get_shares(blob_id, HashSet::new(), HashSet::new(), true)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(executor_shares.len() == 2);
}

#[tokio::test]
#[serial]
#[ignore = "Reenable after transaction mempool is used"]
async fn test_get_commitments_from_peers() {
    let interconnected_topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;
    let executor = &interconnected_topology.executors()[0];

    interconnected_topology.wait_network_ready().await;
    interconnected_topology
        .wait_membership_ready_for_session(SessionNumber::from(0u64))
        .await;

    // Create independent node that only knows about membership of
    // `interconnected_topology` nodes. This validator will not receive any data
    // from the previous two, so it will need to query the DA network over the
    // sampling protocol for the share commitments.
    let lone_general_config = create_general_configs(1).into_iter().next().unwrap();
    let lone_validator_config = create_validator_config(lone_general_config);
    let lone_validator = Validator::spawn(lone_validator_config).await.unwrap();

    let (test_channel_id, parent_msg_id) = setup_test_channel(executor).await;

    let data = [1u8; 31];
    let blob_id = disseminate_with_metadata(executor, test_channel_id, parent_msg_id, &data)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;

    let _ = wait_for_blob_onchain(executor, test_channel_id, blob_id).await;

    lone_validator.get_commitments(blob_id, 0).await.unwrap();

    let timeout = adjust_timeout(Duration::from_secs(DA_TESTS_TIMEOUT));
    assert!(
        (tokio::time::timeout(timeout, async {
            lone_validator.get_commitments(blob_id, 0).await
        })
        .await)
            .is_ok(),
        "timed out waiting for share commitments"
    );
}

#[tokio::test]
#[serial]
async fn test_block_peer() {
    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;
    let executor = &topology.executors()[0];

    let blacklisted_peers = executor.blacklisted_peers().await;
    assert!(blacklisted_peers.is_empty());

    topology.wait_membership_ready().await;
    let existing_peer_id = *executor
        .da_get_membership(0u64)
        .await
        .unwrap()
        .addressbook
        .keys()
        .next()
        .unwrap();

    // try block/unblock peer id combinations
    let blocked = executor.block_peer(existing_peer_id.to_string()).await;
    assert!(blocked);

    let blacklisted_peers = executor.blacklisted_peers().await;
    assert_eq!(blacklisted_peers.len(), 1);
    assert_eq!(blacklisted_peers[0], existing_peer_id.to_string());

    let blocked = executor.block_peer(existing_peer_id.to_string()).await;
    assert!(!blocked);

    let unblocked = executor.unblock_peer(existing_peer_id.to_string()).await;
    assert!(unblocked);

    let blacklisted_peers = executor.blacklisted_peers().await;
    assert!(blacklisted_peers.is_empty());

    let unblocked = executor.unblock_peer(existing_peer_id.to_string()).await;
    assert!(!unblocked);

    // try blocking/unblocking non existing peer id
    let mut node_key_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut node_key_bytes);

    let node_key = ed25519::SecretKey::try_from_bytes(&mut node_key_bytes)
        .expect("Failed to generate secret key from bytes");

    let non_existing_peer_id = secret_key_to_peer_id(node_key);
    let blocked = executor.block_peer(non_existing_peer_id.to_string()).await;
    assert!(blocked);

    let unblocked = executor
        .unblock_peer(non_existing_peer_id.to_string())
        .await;
    assert!(unblocked);
}

#[tokio::test]
#[serial]
async fn test_get_shares() {
    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;
    topology.wait_membership_ready().await;
    topology.wait_network_ready().await;
    topology.wait_da_network_ready().await;

    let executor = &topology.executors()[0];
    let (channel_id, parent_msg_id) = setup_test_channel(executor).await;
    let num_subnets = executor.config().da_network.backend.num_subnets as usize;

    let data = [1u8; 31];
    let blob_id = disseminate_with_metadata(executor, channel_id, parent_msg_id, &data)
        .await
        .unwrap();

    let _ = wait_for_blob_onchain(executor, channel_id, blob_id).await;

    // Wait for transactions to be stored
    tokio::time::sleep(Duration::from_secs(2)).await;

    let exec_url = Url::parse(&format!(
        "http://{}",
        executor.config().http.backend_settings.address
    ))
    .unwrap();
    let client = CommonHttpClient::new(None);

    // Test case 1: Request all shares
    let shares_stream = client
        .get_shares::<DaShare>(
            exec_url.clone(),
            blob_id,
            HashSet::new(),
            HashSet::new(),
            true,
        )
        .await
        .unwrap();
    let shares = shares_stream.collect::<Vec<_>>().await;
    assert_eq!(shares.len(), num_subnets);
    assert!(shares.iter().any(|share| share.share_idx() == [0, 0]));
    assert!(shares.iter().any(|share| share.share_idx() == [1, 0]));

    // Test case 2: Request only the first share
    let shares_stream = client
        .get_shares::<DaShare>(
            exec_url.clone(),
            blob_id,
            HashSet::from([[0, 0]]),
            HashSet::new(),
            false,
        )
        .await
        .unwrap();
    let shares = shares_stream.collect::<Vec<_>>().await;
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].share_idx(), [0, 0]);

    // Test case 3: Request only the first share but return all available
    // shares
    let shares_stream = client
        .get_shares::<DaShare>(
            exec_url.clone(),
            blob_id,
            HashSet::from([[0, 0]]),
            HashSet::new(),
            true,
        )
        .await
        .unwrap();
    let shares = shares_stream.collect::<Vec<_>>().await;
    assert_eq!(shares.len(), num_subnets);
    assert!(shares.iter().any(|share| share.share_idx() == [0, 0]));
    assert!(shares.iter().any(|share| share.share_idx() == [1, 0]));

    // Test case 4: Request all shares and filter out the second share
    let shares_stream = client
        .get_shares::<DaShare>(
            exec_url.clone(),
            blob_id,
            HashSet::new(),
            HashSet::from([[1, 0]]),
            true,
        )
        .await
        .unwrap();
    let shares = shares_stream.collect::<Vec<_>>().await;
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].share_idx(), [0, 0]);

    // Test case 5: Request unavailable shares
    let shares_stream = client
        .get_shares::<DaShare>(
            exec_url.clone(),
            blob_id,
            HashSet::from([[2, 0]]),
            HashSet::new(),
            false,
        )
        .await
        .unwrap();

    let shares = shares_stream.collect::<Vec<_>>().await;
    assert!(shares.is_empty());
}
