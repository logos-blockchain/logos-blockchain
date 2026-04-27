use std::{num::NonZero, time::Duration};

use lb_core::mantle::{
    GenesisTx as _, NoteId,
    gas::GasCost,
    ledger::Inputs,
    ops::channel::{ChannelId, deposit::DepositOp},
};
use lb_http_api_common::bodies::{
    channel::ChannelDepositRequestBody, wallet::balance::WalletBalanceResponseBody,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_node::config::RunConfig;
use lb_testing_framework::{
    DeploymentBuilder, NodeHttpClient, TopologyConfig as TfTopologyConfig,
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::common::manual_cluster::{
    ManualNodeLayout, api_url, get_wallet_balance, start_local_manual_cluster_with_layout,
    wait_for_nodes_height,
};
use testing_framework_core::scenario::DynError;
use tokio::time::sleep;

/// End-to-end test for the channel deposit flow:
///
/// 1. Spawn validators that produce blocks.
/// 2. Call the POST `/channel/deposit` HTTP endpoint on one validator.
/// 3. Verify the API call succeeds.
/// 4. Wait for the deposit transaction to be included in a block.
/// 5. Verify the funding key's wallet balance decreases.
/// 6. Verify the channel balance increases.
#[tokio::test]
async fn channel_deposit() {
    let deposit_amount = 1;
    let (wallet_config, funding_pk) = channel_deposit_wallet_config(deposit_amount, 100);
    let (base, nodes) = start_local_manual_cluster_with_layout(
        "channel-deposit",
        "mantle-channel",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(2)
                .with_allow_multiple_genesis_tokens(true)
                .with_test_context(Some("channel_deposit".to_owned())),
        )
        .with_wallet_config(wallet_config),
        2,
        ManualNodeLayout::SelectNodeSeed(0),
        |config| Ok::<_, DynError>(channel_test_config(config)),
    )
    .await;

    let validator = &nodes[0];

    wait_for_nodes_height(
        nodes
            .iter()
            .map(|node| &node.client)
            .collect::<Vec<_>>()
            .as_slice(),
        3,
        Duration::from_mins(5),
    )
    .await;

    let balance_before = get_wallet_balance(&validator.client, funding_pk).await;

    // Also, record the channel balance before deposit
    // We use the channel created by the genesis inscription for simplicity.
    let channel_id = base
        .deployment
        .config
        .genesis_block
        .expect("manual-cluster deployment should include genesis tx")
        .genesis_tx()
        .genesis_inscription()
        .channel_id;
    let channel_balance_before = get_channel_balance(&validator.client, channel_id).await;
    println!("Channel balance before deposit: {channel_balance_before}");

    let (note_id, selected_deposit_amount) =
        get_wallet_note(&validator.client, funding_pk, deposit_amount).await;
    let body = ChannelDepositRequestBody {
        tip: None,
        deposit: DepositOp {
            channel_id,
            inputs: Inputs::new(vec![note_id]),
            metadata: format!("Mint {selected_deposit_amount} to Alice in Zone").into_bytes(),
        },
        change_public_key: funding_pk,
        funding_public_keys: vec![funding_pk],
        max_tx_fee: GasCost::new(10),
    };
    let response = reqwest::Client::new()
        .post(api_url(&validator.client, "channel/deposit"))
        .json(&body)
        .send()
        .await
        .expect("request should not fail");

    assert!(
        response.status().is_success(),
        "request should succeed, got status: {} body: {}",
        response.status(),
        response.text().await.unwrap_or_default(),
    );

    wait_for_nodes_height(
        nodes
            .iter()
            .map(|node| &node.client)
            .collect::<Vec<_>>()
            .as_slice(),
        8,
        Duration::from_mins(5),
    )
    .await;

    let balance_after = get_wallet_balance(&validator.client, funding_pk).await;
    assert_eq!(
        balance_after,
        balance_before - deposit_amount,
        "wallet balance should decrease after deposit: before={balance_before}, after={balance_after}, deposit_amount={deposit_amount}",
    );

    let channel_balance_after = get_channel_balance(&validator.client, channel_id).await;
    assert_eq!(
        channel_balance_after,
        channel_balance_before + deposit_amount,
        "channel balance should increase after deposit: before={channel_balance_before}, after={channel_balance_after}, deposit_amount={deposit_amount}",
    );
}

fn channel_deposit_wallet_config(
    deposit_note_amount: u64,
    fee_note_amount: u64,
) -> (WalletConfig, ZkPublicKey) {
    let deposit_note = WalletAccount::deterministic(0, deposit_note_amount, false)
        .expect("deposit wallet should be valid");

    let fee_note = WalletAccount::new(
        "channel-deposit-fee-note".to_owned(),
        deposit_note.secret_key.clone(),
        fee_note_amount,
        false,
    )
    .expect("fee wallet should be valid");
    let funding_pk = deposit_note.public_key();

    (WalletConfig::new(vec![deposit_note, fee_note]), funding_pk)
}

fn channel_test_config(mut config: RunConfig) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(1);
    config.deployment.cryptarchia.security_param = NonZero::new(3).unwrap();
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().unwrap());
    config
}

async fn get_wallet_note(node: &NodeHttpClient, pk: ZkPublicKey, min_value: u64) -> (NoteId, u64) {
    let pk_hex = hex::encode(lb_groth16::fr_to_bytes(&pk.into()));
    let url = api_url(node, &format!("wallet/{pk_hex}/balance"));

    let response = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .expect("balance request should not fail");

    assert!(
        response.status().is_success(),
        "balance request should succeed, got status: {}",
        response.status(),
    );

    let body: WalletBalanceResponseBody = response
        .json()
        .await
        .expect("balance response should be valid JSON");

    body.notes
        .into_iter()
        .filter(|(_, value)| *value >= min_value)
        .min_by_key(|(_, value)| *value)
        .expect("should find a note with sufficient balance for deposit")
}

async fn get_channel_balance(node: &NodeHttpClient, channel_id: ChannelId) -> u64 {
    let url = api_url(node, &format!("channel/{channel_id}"));

    for _ in 0..5 {
        let response = reqwest::Client::new()
            .get(url.clone())
            .send()
            .await
            .expect("channel request should not fail");

        if response.status().is_success() {
            let body: serde_json::Value = response.json().await.unwrap();
            return body["balance"].as_u64().unwrap_or(0);
        }

        sleep(Duration::from_millis(500)).await;
    }

    panic!("failed to get channel state after retries");
}
