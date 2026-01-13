use futures::StreamExt as _;
use nomos_core::{da::BlobId, sdp::SessionNumber};
use tests::{
    common::da::{disseminate_with_metadata, setup_test_channel, wait_for_blob_onchain},
    nodes::executor::Executor,
    topology::{Topology, TopologyConfig},
};
use tokio::time::Duration;

#[tokio::test]
async fn test_historical_sampling_across_sessions() {
    let topology = Topology::spawn(TopologyConfig::validator_and_executor()).await;
    let executor = &topology.executors()[0];

    topology.wait_membership_ready().await;
    topology.wait_network_ready().await;
    topology.wait_da_network_ready().await;

    // Disseminate some blobs in session 0
    tokio::time::sleep(Duration::from_secs(15)).await;
    let blob_ids = disseminate_blobs_in_session_zero(executor).await;

    // todo: Blocks 1-4: Complete session 0 and form session 1 on ALL nodes

    // todo: add more complex cases with multiple sessions

    // Wait for propagation
    tokio::time::sleep(Duration::from_secs(5)).await;

    test_sampling_scenarios(executor, &blob_ids, 0).await;
}

async fn disseminate_blobs_in_session_zero(executor: &Executor) -> Vec<BlobId> {
    let mut blob_ids = Vec::new();

    let (test_channel_id, mut parent_msg_id) = setup_test_channel(executor).await;

    let data = [1u8; 31];

    for _ in 0..3 {
        let blob_id = disseminate_with_metadata(executor, test_channel_id, parent_msg_id, &data)
            .await
            .expect("Failed to disseminate blob");

        blob_ids.push(blob_id);

        parent_msg_id = wait_for_blob_onchain(executor, test_channel_id, blob_id).await;

        verify_share_replication(executor, blob_id).await;
    }

    blob_ids
}

async fn test_sampling_scenarios(executor: &Executor, blob_ids: &[BlobId], session: SessionNumber) {
    let block_id = [0u8; 32];

    let valid_future = async {
        let blob_ids_map: Vec<(BlobId, SessionNumber)> =
            blob_ids.iter().map(|&blob_id| (blob_id, session)).collect();

        let result = executor
            .da_historic_sampling(block_id.into(), blob_ids_map)
            .await
            .expect("HTTP request should succeed");
        assert!(
            result,
            "Historical sampling should return true for session 0 where data exists"
        );
    };

    // Test 2: Mixed valid/invalid blobs - all from session 0
    let invalid_future = async {
        let block_id = [1u8; 32];
        let mut mixed_blob_ids = blob_ids.to_vec();
        mixed_blob_ids[0] = [99u8; 32];

        let blob_ids_map: Vec<(BlobId, SessionNumber)> =
            mixed_blob_ids.iter().map(|&blob_id| (blob_id, 0)).collect();

        let result = executor
            .da_historic_sampling(block_id.into(), blob_ids_map)
            .await
            .expect("HTTP request should succeed");
        assert!(
            !result,
            "Historical sampling should return false when any blob is invalid"
        );
    };

    // Run both tests concurrently
    tokio::join!(valid_future, invalid_future);
}

async fn verify_share_replication(executor: &Executor, blob_id: BlobId) {
    let shares = executor
        .get_shares(blob_id, [].into(), [].into(), true)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(!shares.is_empty(), "Should have replicated shares");
}
