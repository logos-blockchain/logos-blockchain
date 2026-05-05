use std::{
    num::NonZero,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures::StreamExt as _;
use lb_api_service::http::consensus::leader::LeaderClaimResponseBody;
use lb_chain_service::{ChainServiceMode, State};
use lb_node::{
    Transaction as _, TxHash,
    config::{RunConfig, cryptarchia::deployment::EpochConfig},
};
use lb_testing_framework::{DeploymentBuilder, NodeHttpClient, TopologyConfig as TfTopologyConfig};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::common::manual_cluster::{
    ManualNodeLayout, api_url, start_local_manual_cluster_with_layout,
};
use testing_framework_core::scenario::DynError;
use tokio::time::{sleep, timeout};

const NODE_COUNT: usize = 1;

/// End-to-end test for the leader claim flow:
///
/// 1. Spawn a node that produce blocks.
/// 2. Wait for enough slot progress to cross epoch boundaries.
/// 3. Call the POST `/leader/claim` HTTP endpoint on the node.
/// 4. Verify the claim tx is successfully included in the chain.
#[tokio::test]
async fn leader_claim() {
    // TODO: This is an workaround to get values from the updated configs
    // during the cluster setup. Refactor the testing framework for better UX.
    let slots_per_epoch = Arc::new(AtomicU64::new(0));
    let (_base, nodes) = start_local_manual_cluster_with_layout(
        "leader-claim",
        "mantle-leader",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(NODE_COUNT)
                .with_test_context(Some("leader_claim".to_owned())),
        ),
        NODE_COUNT,
        ManualNodeLayout::SelectNodeSeed(0),
        {
            let slots_per_epoch = Arc::clone(&slots_per_epoch);
            move |config| Ok::<_, DynError>(test_config(config, &slots_per_epoch))
        },
    )
    .await;
    let slots_per_epoch = slots_per_epoch.load(Ordering::Relaxed);

    let node = &nodes[0];

    // Wait for two epoch transitions.
    // 0->1: vouchers (blocks) are collected but not added to MMR
    // 1->2: vouchers are added to MMR and become claimable
    let target_slot = 2 * slots_per_epoch;
    wait_for_nodes_slot(
        nodes
            .iter()
            .map(|node| &node.client)
            .collect::<Vec<_>>()
            .as_slice(),
        target_slot,
        Duration::from_mins(5),
    )
    .await;

    // Subscribes to process blocks to check if our tx is included in the end
    let mut block_stream = node.client.blocks_stream().await.unwrap();

    // Submit a tx with a LeaderClaim operation
    let tx_hash = claim_leader_rewards(&node.client, Duration::from_secs(30)).await;

    // Wait for the claim tx to be included in the chain
    // TODO: Check if wallet balance is increased by improving wallet
    // to track reward UTXOs in the wallet: https://github.com/logos-blockchain/logos-blockchain/issues/2627
    timeout(Duration::from_mins(1), {
        async {
            while let Some(block) = block_stream.next().await {
                if block
                    .block
                    .transactions
                    .iter()
                    .any(|tx| tx.hash() == tx_hash)
                {
                    break;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Timed out waiting for tx to be included in the chain"));
}

fn test_config(mut config: RunConfig, slots_per_epoch: &AtomicU64) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(1);
    config.deployment.cryptarchia.epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    config.deployment.cryptarchia.security_param = NonZero::new(2).unwrap();
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().unwrap());

    slots_per_epoch.store(
        config.deployment.cryptarchia.slots_per_epoch(),
        Ordering::Relaxed,
    );

    config
}

async fn claim_leader_rewards(node: &NodeHttpClient, duration: Duration) -> TxHash {
    timeout(duration, async {
        loop {
            let response = reqwest::Client::new()
                .post(api_url(node, "leader/claim"))
                .send()
                .await
                .expect("leader claim request should not fail");

            let status = response.status();
            if status.is_success() {
                let body: LeaderClaimResponseBody = response
                    .json()
                    .await
                    .expect("leader claim response should be valid JSON");
                return body.tx_hash;
            }

            let body = response.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::INTERNAL_SERVER_ERROR
                && body.contains("No claimable voucher found")
            {
                sleep(Duration::from_millis(500)).await;
                continue;
            }

            panic!("leader claim should succeed, got status: {status} body: {body}");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("leader claim should become available within {duration:?}"))
}

async fn wait_for_nodes_slot(nodes: &[&NodeHttpClient], target_slot: u64, duration: Duration) {
    timeout(duration, async {
        loop {
            let mut all_ready = true;

            for node in nodes {
                let info = node
                    .consensus_info()
                    .await
                    .expect("fetching consensus info should succeed");

                if info.cryptarchia_info.slot < target_slot.into()
                    || !matches!(info.mode, ChainServiceMode::Started(State::Online))
                {
                    all_ready = false;
                    break;
                }
            }

            if all_ready {
                return;
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("nodes should reach slot {target_slot}"));
}
