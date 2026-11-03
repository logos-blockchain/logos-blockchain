use std::{collections::HashSet, num::NonZero, path::PathBuf, time::Duration};

use futures::StreamExt as _;
use lb_common_http_client::ProcessedBlockEvent;
use lb_core::mantle::transactions::UNITS_PER_LGO;
use lb_groth16::fr_to_bytes;
use lb_http_api_common::bodies::wallet::balance::WalletBalanceResponseBody;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_node::config::{RunConfig, deployment::era::ruleset::v1::cryptarchia::EpochConfig};
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, NodeHttpClient, TopologyConfig as TfTopologyConfig,
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::{
    common::manual_cluster::{
        LocalManualClusterHarnessBase, ManualNodeLayout, api_url,
        start_local_manual_cluster_with_layout,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use testing_framework_core::scenario::{DynError, StartedNode};
use tokio::time::{sleep, timeout};

const NODE_COUNT: usize = 1;

/// The leader reward is a note of each block, owned by the reward key the
/// header carries; the leader's wallet adopts it.
#[tokio::test]
async fn leader_rewards_reach_the_leader_wallet() {
    let (_base, nodes, _leader_funding_pk) =
        setup_test_nodes("leader_rewards_reach_the_leader_wallet").await;
    let node = &nodes[0];
    let mut block_stream = node.client.blocks_stream().await.unwrap();
    let mut leader_keys = HashSet::new();
    collect_reward_keys(&mut block_stream, &mut leader_keys, 1).await;
    let before = total_balance(&node.client, &leader_keys).await;
    collect_reward_keys(&mut block_stream, &mut leader_keys, 5).await;
    wait_for_total_balance_above(&node.client, &leader_keys, before, Duration::from_mins(1)).await;
}

async fn collect_reward_keys(
    block_stream: &mut (impl futures::Stream<Item = ProcessedBlockEvent> + Unpin),
    reward_keys: &mut HashSet<ZkPublicKey>,
    blocks: usize,
) {
    timeout(Duration::from_mins(2), async {
        for _ in 0..blocks {
            let block = block_stream
                .next()
                .await
                .expect("block stream should not close");
            reward_keys.insert(*block.block.header.proof_of_leadership.reward_key());
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the node should produce {blocks} blocks"));
}

async fn total_balance(node: &NodeHttpClient, pks: &HashSet<ZkPublicKey>) -> u64 {
    let mut total = 0;
    for pk in pks {
        let pk_hex = hex::encode(fr_to_bytes(&(*pk).into()));
        let response = reqwest::Client::new()
            .get(api_url(node, &format!("wallet/{pk_hex}/balance")))
            .send()
            .await
            .expect("balance request should not fail");
        // A key whose reward note the wallet has not adopted yet is not found.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            continue;
        }
        assert!(
            response.status().is_success(),
            "balance request should succeed, got status: {}",
            response.status()
        );
        let balance: WalletBalanceResponseBody = response
            .json()
            .await
            .expect("balance response should be valid JSON");
        total += balance.balance;
    }
    total
}

async fn wait_for_total_balance_above(
    node: &NodeHttpClient,
    pks: &HashSet<ZkPublicKey>,
    before: u64,
    duration: Duration,
) {
    timeout(duration, async {
        while total_balance(node, pks).await <= before {
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the leader's wallet balance should grow above {before}"));
}

fn test_config(mut config: RunConfig, leader_funding_pk: ZkPublicKey) -> RunConfig {
    config
        .deployment
        .update_genesis_era_parameters(|parameters| {
            parameters.time.slot_duration_in_seconds = 1.try_into().unwrap();
            parameters.cryptarchia.epoch_config = EpochConfig {
                epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
                epoch_period_nonce_buffer: 1.try_into().unwrap(),
                epoch_period_nonce_stabilization: 1.try_into().unwrap(),
            };
            parameters.cryptarchia.security_param = NonZero::new(2).unwrap();
            parameters.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap())
                    .try_into()
                    .unwrap();
            // The widest uncle reference window k allows, ⌊0.6k⌋.
            parameters.cryptarchia.uncle_reference_window_in_block = 1;
        })
        .expect("The changed deployment must be valid.");
    config.user.cryptarchia.leader.wallet.funding_pk = leader_funding_pk;

    config
}

async fn setup_test_nodes(
    test_context: &str,
) -> (
    LocalManualClusterHarnessBase,
    Vec<StartedNode<LbcEnv>>,
    ZkPublicKey,
) {
    let (wallet_config, leader_funding_pk) = leader_funding_wallet_config();
    let (base, nodes) = start_local_manual_cluster_with_layout(
        "leader-claim",
        "mantle-leader",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(NODE_COUNT)
                .with_allow_multiple_genesis_tokens(true)
                .with_test_context(Some(test_context.to_owned())),
        )
        .with_wallet_config(wallet_config),
        NODE_COUNT,
        ManualNodeLayout::SelectNodeSeed(0),
        move |config| Ok::<_, DynError>(test_config(config, leader_funding_pk)),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    )
    .await;

    (base, nodes, leader_funding_pk)
}

fn leader_funding_wallet_config() -> (WalletConfig, ZkPublicKey) {
    let account = WalletAccount::deterministic(42, 100_000 * UNITS_PER_LGO, false)
        .expect("leader funding account should be valid");
    let funding_pk = account.public_key();
    let accounts = (0..3)
        .map(|idx| {
            WalletAccount::new(
                format!("leader-funding-{idx}"),
                account.secret_key.clone(),
                100_000 * UNITS_PER_LGO,
                false,
            )
            .expect("leader funding account should be valid")
        })
        .collect();

    (WalletConfig::new(accounts), funding_pk)
}
