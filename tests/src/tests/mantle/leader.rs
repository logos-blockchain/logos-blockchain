use std::{num::NonZero, time::Duration};

use lb_cryptarchia_engine::{base_period_length, time::epoch_length};
use lb_node::config::{RunConfig, cryptarchia::deployment::EpochConfig};
use lb_testing_framework::{DeploymentBuilder, NodeHttpClient, TopologyConfig as TfTopologyConfig};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::common::manual_cluster::{
    ManualNodeLayout, api_url, get_wallet_balance, start_local_manual_cluster_with_layout,
    wait_for_nodes_height,
};
use testing_framework_core::scenario::DynError;
use tokio::time::{sleep, timeout};

/// End-to-end test for the leader claim flow:
///
/// 1. Spawn validators that produce blocks.
/// 2. Wait for enough slot progress to cross epoch boundaries.
/// 3. Call the POST `/leader/claim` HTTP endpoint on one validator.
/// 4. Verify the claim succeeds.
/// 5. Wait for the claim transaction to be included in a block.
/// 6. Verify the funding key's wallet balance increases.
#[tokio::test]
async fn leader_claim() {
    let (base, nodes) = start_local_manual_cluster_with_layout(
        "leader-claim",
        "mantle-leader",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(2)
                .with_test_context(Some("leader_claim".to_owned())),
        ),
        2,
        ManualNodeLayout::SelectNodeSeed(0),
        |config| Ok::<_, DynError>(leader_test_config(config)),
    )
    .await;

    let validator = &nodes[0];
    let funding_pk = base.deployment.nodes()[0]
        .general
        .consensus_config
        .funding_pk;

    let target_slot = 3 * leader_slots_per_epoch();
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

    let balance_before = get_wallet_balance(&validator.client, funding_pk).await;

    claim_leader_rewards(&validator.client, Duration::from_secs(30)).await;

    let tip_height = validator
        .client
        .consensus_info()
        .await
        .expect("fetching consensus info should succeed")
        .height;
    wait_for_nodes_height(
        nodes
            .iter()
            .map(|node| &node.client)
            .collect::<Vec<_>>()
            .as_slice(),
        tip_height + 5,
        Duration::from_mins(5),
    )
    .await;

    let balance_after = get_wallet_balance(&validator.client, funding_pk).await;
    assert!(
        balance_after > balance_before,
        "balance should increase after claiming rewards: before={balance_before}, after={balance_after}",
    );
}

fn leader_test_config(mut config: RunConfig) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(1);
    config.deployment.cryptarchia.epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    config.deployment.cryptarchia.security_param = NonZero::new(2).unwrap();
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().unwrap());
    config
}

fn leader_slots_per_epoch() -> u64 {
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    let security_param = NonZero::new(2).unwrap();
    let slot_activation_coeff = NonNegativeRatio::new(1, 2.try_into().unwrap());

    epoch_length(
        epoch_config.epoch_stake_distribution_stabilization,
        epoch_config.epoch_period_nonce_buffer,
        epoch_config.epoch_period_nonce_stabilization,
        base_period_length(security_param, slot_activation_coeff),
    )
}

async fn claim_leader_rewards(node: &NodeHttpClient, duration: Duration) {
    timeout(duration, async {
        loop {
            let response = reqwest::Client::new()
                .post(api_url(node, "leader/claim"))
                .send()
                .await
                .expect("leader claim request should not fail");

            let status = response.status();
            let body = response.text().await.unwrap_or_default();

            if status.is_success() {
                return;
            }

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
    .unwrap_or_else(|_| panic!("leader claim should become available within {duration:?}"));
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

                if info.slot < target_slot.into()
                    || info.mode != lb_cryptarchia_engine::State::Online
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
