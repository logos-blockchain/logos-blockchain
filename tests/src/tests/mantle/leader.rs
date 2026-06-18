use std::{collections::HashSet, num::NonZero, path::PathBuf, time::Duration};

use futures::StreamExt as _;
use lb_api_service::http::consensus::leader::LeaderClaimResponseBody;
use lb_common_http_client::ProcessedBlockEvent;
use lb_groth16::fr_to_bytes;
use lb_http_api_common::bodies::wallet::{
    balance::WalletBalanceResponseBody, claimable_vouchers::WalletClaimableVouchersResponseBody,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_node::{
    Transaction as _, TxHash,
    config::{RunConfig, cryptarchia::deployment::EpochConfig},
};
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

/// End-to-end test for the leader claim flow:
///
/// 1. Spawn a node that produce blocks.
/// 2. Wait for enough slot progress to cross epoch boundaries.
/// 3. Call the POST `/leader/claim` HTTP endpoint on the node.
/// 4. Verify the claim tx is successfully included in the chain.
#[tokio::test]
async fn leader_claim() {
    let (_base, nodes, _leader_funding_pk) = setup_test_nodes("leader_claim").await;
    let node = &nodes[0];
    let mut block_stream = node.client.blocks_stream().await.unwrap();

    // Wait for two epoch transitions.
    // 0->1: vouchers (blocks) are collected but not added to MMR
    // 1->2: vouchers are added to MMR and become claimable
    // Submit a tx with a LeaderClaim operation
    wait_for_claimable_vouchers(&node.client, Duration::from_mins(5)).await;

    let tx_hash = claim_leader_rewards(&node.client, Duration::from_secs(30)).await;

    // Wait for the claim tx to be included in the chain
    // TODO: Check if wallet balance is increased by improving wallet
    // to track reward UTXOs in the wallet: https://github.com/logos-blockchain/logos-blockchain/issues/2627
    wait_for_tx_inclusion(&mut block_stream, tx_hash).await;
}

fn test_config(mut config: RunConfig, leader_funding_pk: ZkPublicKey) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(1);
    config.deployment.cryptarchia.epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    config.deployment.cryptarchia.security_param = NonZero::new(2).unwrap();
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().unwrap());
    config.user.cryptarchia.leader.wallet.funding_pk = leader_funding_pk;

    config
}

async fn get_claimable_vouchers(node: &NodeHttpClient) -> WalletClaimableVouchersResponseBody {
    let response = reqwest::Client::new()
        .get(api_url(node, "leader/claim/vouchers"))
        .send()
        .await
        .expect("claimable vouchers request should not fail");

    assert!(
        response.status().is_success(),
        "claimable vouchers request should succeed, got status: {} body: {}",
        response.status(),
        response.text().await.unwrap_or_default(),
    );

    response
        .json()
        .await
        .expect("claimable vouchers response should be valid JSON")
}

async fn wait_for_claimable_vouchers(node: &NodeHttpClient, duration: Duration) {
    timeout(duration, async {
        loop {
            let claimable_vouchers = get_claimable_vouchers(node).await;

            if !claimable_vouchers.vouchers.is_empty() {
                return;
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("leader should have claimable vouchers within {duration:?}"));
}

async fn claim_leader_rewards(node: &NodeHttpClient, duration: Duration) -> TxHash {
    timeout(duration, async {
        loop {
            match try_claim_leader_rewards_once(node).await {
                Ok(Some(body)) => return body.tx_hash,
                Ok(None) => sleep(Duration::from_millis(500)).await,
                Err(err) => panic!("{err}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("leader claim should become available within {duration:?}"))
}

#[tokio::test]
async fn concurrent_leader_claims() {
    let (_base, nodes, leader_funding_pk) = setup_test_nodes("concurrent_leader_claims").await;
    let node = &nodes[0];
    let mut block_stream = node.client.blocks_stream().await.unwrap();

    wait_for_wallet_notes(&node.client, leader_funding_pk, 2, Duration::from_mins(2)).await;
    let successful_tx_hashes =
        submit_concurrent_leader_claims(&node.client, 2, Duration::from_mins(1)).await;
    assert_unique_tx_hashes(&successful_tx_hashes);

    wait_for_tx_inclusions(&mut block_stream, &successful_tx_hashes).await;
}

async fn submit_concurrent_leader_claims(
    node: &NodeHttpClient,
    min_successful_claims: usize,
    duration: Duration,
) -> Vec<TxHash> {
    timeout(duration, async {
        loop {
            let (tx1, tx2, tx3) = tokio::join!(
                try_claim_leader_rewards_once(node),
                try_claim_leader_rewards_once(node),
                try_claim_leader_rewards_once(node)
            );
            let tx_hashes = <[_; 3]>::from((tx1, tx2, tx3))
                .into_iter()
                .filter_map(|result| {
                    result.expect(
                        "leader claim request should either reserve a voucher or report none",
                    )
                })
                .map(|body| body.tx_hash)
                .collect::<Vec<_>>();

            if tx_hashes.len() >= min_successful_claims {
                return tx_hashes;
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("leader claim should become available within {duration:?}"))
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
    let account = WalletAccount::deterministic(42, 100_000, false)
        .expect("leader funding account should be valid");
    let funding_pk = account.public_key();
    let accounts = (0..3)
        .map(|idx| {
            WalletAccount::new(
                format!("leader-funding-{idx}"),
                account.secret_key.clone(),
                100_000,
                false,
            )
            .expect("leader funding account should be valid")
        })
        .collect();

    (WalletConfig::new(accounts), funding_pk)
}

async fn wait_for_tx_inclusion(
    block_stream: &mut (impl futures::Stream<Item = ProcessedBlockEvent> + Unpin),
    tx_hash: TxHash,
) {
    wait_for_tx_inclusions(block_stream, &[tx_hash]).await;
}

async fn wait_for_tx_inclusions(
    block_stream: &mut (impl futures::Stream<Item = ProcessedBlockEvent> + Unpin),
    tx_hashes: &[TxHash],
) {
    let mut remaining_tx_hashes = tx_hashes.iter().copied().collect::<HashSet<_>>();

    timeout(Duration::from_mins(1), async {
        while let Some(block) = block_stream.next().await {
            for tx in &block.block.transactions {
                remaining_tx_hashes.remove(&tx.hash());
            }

            if remaining_tx_hashes.is_empty() {
                return;
            }
        }

        panic!("block stream closed before txs were included; remaining={remaining_tx_hashes:?}");
    })
    .await
    .unwrap_or_else(|_| {
        panic!("Timed out waiting for txs to be included; remaining={remaining_tx_hashes:?}")
    });
}

fn assert_unique_tx_hashes(tx_hashes: &[TxHash]) {
    let unique = tx_hashes.iter().collect::<HashSet<_>>();
    assert_eq!(
        unique.len(),
        tx_hashes.len(),
        "Concurrent claims should not return duplicate tx hashes"
    );
}

async fn wait_for_wallet_notes(
    node: &NodeHttpClient,
    pk: ZkPublicKey,
    min_notes: usize,
    duration: Duration,
) {
    timeout(duration, async {
        loop {
            let balance = get_wallet_balance(node, pk).await;

            if balance.notes.len() >= min_notes {
                return;
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("wallet should have at least {min_notes} funding notes"));
}

async fn get_wallet_balance(node: &NodeHttpClient, pk: ZkPublicKey) -> WalletBalanceResponseBody {
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

async fn try_claim_leader_rewards_once(
    node: &NodeHttpClient,
) -> Result<Option<LeaderClaimResponseBody>, String> {
    let response = reqwest::Client::new()
        .post(api_url(node, "leader/claim"))
        .send()
        .await
        .map_err(|err| format!("leader claim request should not fail: {err}"))?;

    let status = response.status();
    if response.status().is_success() {
        return response
            .json()
            .await
            .map(Some)
            .map_err(|err| format!("leader claim response should be valid JSON: {err}"));
    }

    let body = response.text().await.unwrap_or_default();
    if is_claim_temporarily_unavailable(status, &body) {
        return Ok(None);
    }

    Err(format!(
        "leader claim should succeed, got status: {status} body: {body}"
    ))
}

fn is_claim_temporarily_unavailable(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::INTERNAL_SERVER_ERROR
        && (body.contains("No claimable voucher found")
            || body.contains("Wallet does not have enough funds"))
}
