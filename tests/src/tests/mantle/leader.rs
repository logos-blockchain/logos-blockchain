use std::{
    collections::{BTreeMap, HashMap},
    num::NonZero,
    path::PathBuf,
    time::Duration,
};

use lb_config::consensus::UNITS_PER_LGO;
use lb_groth16::{Fr, fr_to_bytes};
use lb_http_api_common::bodies::wallet::{
    balance::WalletBalanceResponseBody, transfer_funds::WalletTransferFundsRequestBody,
};
use lb_key_management_system_service::keys::{Key, ZkKey, ZkPublicKey};
use lb_node::config::{
    RunConfig,
    deployment::{DeploymentSettings, EraDeclarations, era::ruleset::v1::cryptarchia::EpochConfig},
};
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, NodeHttpClient, TopologyConfig as TfTopologyConfig,
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_utils::math::NonNegativeRatio;
use lb_wallet_service::{LeadershipCredentials, LeadershipKeyIds, LeadershipPublicKeys};
use logos_blockchain_tests::{
    common::manual_cluster::{
        LocalManualClusterHarnessBase, ManualNodeLayout, api_url,
        start_local_manual_cluster_with_layout, wait_for_height,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use testing_framework_core::scenario::{DynError, StartedNode};
use tokio::time::{sleep, timeout};

const NODE_COUNT: usize = 1;

/// No spending credential can account for the delegated note's leadership.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "The fixture proves delegated leadership across restart"
)]
async fn delegated_leadership_survives_node_restart_without_spending_authority() {
    let nf = ZkKey::from_direct_secret(Fr::from(7u64));
    let staking = ZkKey::from_direct_secret(Fr::from(19u64));
    let public = LeadershipPublicKeys {
        nullifier_public_key: nf.to_public_key(),
        staking_public_key: staking.to_public_key(),
    };
    let reward = ZkKey::new(Fr::from(500u64));
    let reward_pk = reward.to_public_key();
    let (wallet_config, _, _) = leader_funding_wallet_config();
    let (base, nodes) = start_local_manual_cluster_with_layout(
        "delegated-leader",
        "mantle-leader",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(1)
                .with_allow_multiple_genesis_tokens(true)
                .with_test_context(Some("delegated-leader".into())),
        )
        .with_wallet_config(wallet_config),
        1,
        ManualNodeLayout::SelectNodeSeed(0),
        move |config| {
            use lb_core::block::genesis::GenesisBlockBuilder;

            let mut config = test_config(config, reward_pk);
            config
                .deployment
                .update_genesis_era_parameters(|parameters| {
                    // Keep reward notes immature for the duration of this test.
                    parameters
                        .cryptarchia
                        .epoch_config
                        .epoch_stake_distribution_stabilization = 200.try_into().unwrap();
                })?;
            let old = config.deployment.genesis_block().genesis_tx();
            let mut allocations = old.distribution().iter().copied().collect::<Vec<_>>();
            let largest = allocations
                .iter_mut()
                .max_by_key(|allocation| allocation.value)
                .unwrap();
            let delegated_stake = (largest.value / 10) * 9;
            largest.value -= delegated_stake;
            let mut declarations = old
                .sdp_declarations()
                .map(|entry| entry.operation().clone())
                .collect::<Vec<_>>();
            let declaration = declarations
                .first_mut()
                .expect("one genesis SDP declaration");
            declaration.note_pk = public.nullifier_public_key;
            declaration.note_stake_pk = public.staking_public_key;
            declaration.amount += delegated_stake;
            let genesis = GenesisBlockBuilder::new()
                .try_add_notes(allocations)?
                .set_inscription(old.inscription().operation().clone())
                .add_declarations(declarations)?
                .build()?;
            let eras = config
                .deployment
                .era_schedule()
                .iter()
                .map(|era| (era.first_epoch, era.parameters.ruleset.clone()))
                .collect::<BTreeMap<_, _>>();
            config.deployment =
                DeploymentSettings::new(&EraDeclarations::try_from(eras)?, genesis)?;
            assert!(config.user.kms.backend.keys.values().all(|key| match key {
                Key::Zk(key) if key.is_spending_key() =>
                    key.to_public_key() != public.nullifier_public_key,
                _ => true,
            }));
            config
                .user
                .kms
                .backend
                .keys
                .insert("delegated-nf".into(), Key::Zk(nf.clone()));
            config
                .user
                .kms
                .backend
                .keys
                .insert("delegated-stake".into(), Key::Zk(staking.clone()));
            config
                .user
                .kms
                .backend
                .keys
                .insert("reward-only".into(), Key::Zk(reward.clone()));
            config.user.wallet.known_keys = HashMap::from([("reward-only".into(), reward_pk)]);
            config.user.pow.auto_claim.targets.clear();
            config.user.wallet.leadership_keys = vec![LeadershipCredentials {
                keys: LeadershipKeyIds {
                    nullifier: "delegated-nf".into(),
                    staking: "delegated-stake".into(),
                },
                public,
            }];
            config.user.sdp.wallet.funding_pk = reward_pk;
            Ok::<_, DynError>(config)
        },
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    )
    .await;
    let node = &nodes[0];
    wait_for_height(&node.client, 2, Duration::from_mins(3))
        .await
        .expect("delegated note should produce blocks");
    let aged = node.client.leader_aged_notes(None).await.unwrap();
    assert_eq!(aged.notes.len(), 1);
    assert_eq!(aged.notes[0].public_key, public.nullifier_public_key);
    assert_eq!(
        aged.notes[0].note_set,
        lb_core::mantle::note_set::NoteSetIndex::Sdp
    );
    let address = hex::encode(fr_to_bytes(public.nullifier_public_key.as_fr()));
    let response = reqwest::get(api_url(&node.client, &format!("wallet/{address}/balance")))
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    let height = node
        .client
        .consensus_info()
        .await
        .unwrap()
        .cryptarchia_info
        .height;
    base.cluster().restart_node(&node.name).await.unwrap();
    base.cluster().wait_node_ready(&node.name).await.unwrap();
    let client = base.cluster().node_client(&node.name).unwrap();
    wait_for_height(&client, height + 2, Duration::from_mins(3))
        .await
        .expect("delegated leadership should resume after restart");
    let aged = client.leader_aged_notes(None).await.unwrap();
    assert_eq!(aged.notes.len(), 1);
    assert_eq!(aged.notes[0].public_key, public.nullifier_public_key);
    let response = reqwest::get(api_url(&client, &format!("wallet/{address}/balance")))
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
}

/// The leader reward is a note of each block, whose key the header hides; the
/// leader records it under its funding key and its wallet adopts it, also
/// across a restart.
#[tokio::test]
async fn leader_rewards_reach_the_leader_wallet() {
    let (base, nodes, leader_funding_pk, payer_pk) =
        setup_test_nodes("leader_rewards_reach_the_leader_wallet").await;
    let node = &nodes[0];
    let before = wallet_balance(&node.client, leader_funding_pk)
        .await
        .balance;
    // The genesis total stake is above its target, so the reserve releases
    // nothing and the block reward is what the pooled fees pay: a
    // transaction funds it, from another key so the fee does not come out
    // of the balance it should grow.
    node.client
        .transfer_funds(WalletTransferFundsRequestBody {
            tip: None,
            change_public_key: payer_pk,
            funding_public_keys: vec![payer_pk],
            recipient_public_key: payer_pk,
            amount: 1,
        })
        .await
        .expect("the transfer should be submitted");
    wait_for_balance_above(
        &node.client,
        leader_funding_pk,
        before,
        Duration::from_mins(3),
    )
    .await;

    // The notes of the blocks above the LIB are not in the state the wallet
    // restarts from: it adopts them again from the openings it persisted.
    let held = wallet_balance(&node.client, leader_funding_pk).await.notes;
    let cluster = base.cluster();
    cluster
        .restart_node(&node.name)
        .await
        .expect("the node should restart");
    cluster
        .wait_node_ready(&node.name)
        .await
        .expect("the node should become ready after its restart");
    let client = cluster
        .node_client(&node.name)
        .expect("the client of the node should be available after its restart");
    timeout(Duration::from_mins(1), async {
        loop {
            let notes = wallet_balance(&client, leader_funding_pk).await.notes;
            if held.keys().all(|commitment| notes.contains_key(commitment)) {
                return;
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("the wallet should hold its reward notes again after the restart");
}

async fn wallet_balance(node: &NodeHttpClient, pk: ZkPublicKey) -> WalletBalanceResponseBody {
    let pk_hex = hex::encode(fr_to_bytes(&pk.into()));
    let response = reqwest::Client::new()
        .get(api_url(node, &format!("wallet/{pk_hex}/balance")))
        .send()
        .await
        .expect("balance request should not fail");
    assert!(
        response.status().is_success(),
        "balance request should succeed, got status: {}",
        response.status()
    );
    response
        .json()
        .await
        .expect("balance response should be valid JSON")
}

async fn wait_for_balance_above(
    node: &NodeHttpClient,
    pk: ZkPublicKey,
    before: u64,
    duration: Duration,
) {
    timeout(duration, async {
        while wallet_balance(node, pk).await.balance <= before {
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
    ZkPublicKey,
) {
    let (wallet_config, leader_funding_pk, payer_pk) = leader_funding_wallet_config();
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

    (base, nodes, leader_funding_pk, payer_pk)
}

fn leader_funding_wallet_config() -> (WalletConfig, ZkPublicKey, ZkPublicKey) {
    let account = WalletAccount::deterministic(42, 100_000 * UNITS_PER_LGO, false)
        .expect("leader funding account should be valid");
    let funding_pk = account.public_key();
    let payer = WalletAccount::deterministic(43, 100_000 * UNITS_PER_LGO, false)
        .expect("payer account should be valid");
    let payer_pk = payer.public_key();
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
        .chain([payer])
        .collect();

    (WalletConfig::new(accounts), funding_pk, payer_pk)
}
