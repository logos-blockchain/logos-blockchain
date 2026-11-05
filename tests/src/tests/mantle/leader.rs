use std::{
    num::NonZero,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use lb_core::{crypto::ZkHasher, mantle::transactions::UNITS_PER_LGO};
use lb_groth16::{Fr, fr_to_bytes};
use lb_http_api_common::bodies::wallet::{
    balance::WalletBalanceResponseBody, transfer_funds::WalletTransferFundsRequestBody,
};
use lb_key_management_system_service::keys::{Key, ZkKey, ZkPublicKey};
use lb_node::config::{RunConfig, deployment::era::ruleset::v1::cryptarchia::EpochConfig};
use lb_poseidon2::Digest as _;
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, NodeHttpClient, TopologyConfig as TfTopologyConfig,
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_utils::math::NonNegativeRatio;
use lb_wallet::LEADER_REWARD_KEY_INDEX;
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

/// The leader reward is a note of each block, whose key the header hides; the
/// leader's wallet finds it under its reward key and adopts it.
#[tokio::test]
async fn leader_rewards_reach_the_leader_wallet() {
    let (_base, nodes, leader_funding_pk, reward_key) =
        setup_test_nodes("leader_rewards_reach_the_leader_wallet").await;
    let node = &nodes[0];
    let before = balance(&node.client, reward_key).await;
    // The genesis total stake is above its target, so the reserve releases nothing
    // and the block reward is what the pooled fees pay: a transaction funds it.
    node.client
        .transfer_funds(WalletTransferFundsRequestBody {
            tip: None,
            change_public_key: leader_funding_pk,
            funding_public_keys: vec![leader_funding_pk],
            recipient_public_key: leader_funding_pk,
            amount: 1,
        })
        .await
        .expect("the transfer should be submitted");
    wait_for_balance_above(&node.client, reward_key, before, Duration::from_mins(3)).await;
}

// The key the wallet of a node pays its leader rewards to.
fn leader_reward_key(config: &RunConfig) -> ZkPublicKey {
    let master_key_id = &config.user.wallet.voucher_master_key_id;
    let Some(Key::Zk(master_key)) = config.user.kms.backend.keys.get(master_key_id) else {
        panic!("the voucher master key is a ZK key of the KMS");
    };
    ZkKey::new(ZkHasher::digest(&[
        *master_key.as_fr(),
        Fr::from(LEADER_REWARD_KEY_INDEX),
    ]))
    .to_public_key()
}

async fn balance(node: &NodeHttpClient, pk: ZkPublicKey) -> u64 {
    let pk_hex = hex::encode(fr_to_bytes(&pk.into()));
    let response = reqwest::Client::new()
        .get(api_url(node, &format!("wallet/{pk_hex}/balance")))
        .send()
        .await
        .expect("balance request should not fail");
    // A key whose reward note the wallet has not adopted yet is not found.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return 0;
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
    balance.balance
}

async fn wait_for_balance_above(
    node: &NodeHttpClient,
    pk: ZkPublicKey,
    before: u64,
    duration: Duration,
) {
    timeout(duration, async {
        while balance(node, pk).await <= before {
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
                NonNegativeRatio::new(1, 2.try_into().unwrap());
        });
    config.user.cryptarchia.leader.wallet.funding_pk = leader_funding_pk;

    config
}

async fn setup_test_nodes(
    test_context: &str,
) -> (
    LocalManualClusterHarnessBase,
    Vec<StartedNode<LbcEnv>>,
    ZkPublicKey,
    ZkPublicKey,
) {
    let (wallet_config, leader_funding_pk) = leader_funding_wallet_config();
    let reward_key = Arc::new(OnceLock::new());
    let node_reward_key = Arc::clone(&reward_key);
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
        move |config| {
            node_reward_key.get_or_init(|| leader_reward_key(&config));
            Ok::<_, DynError>(test_config(config, leader_funding_pk))
        },
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    )
    .await;

    let reward_key = *reward_key
        .get()
        .expect("the node of the test has a configuration");
    (base, nodes, leader_funding_pk, reward_key)
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
