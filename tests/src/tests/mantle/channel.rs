use std::{num::NonZero, time::Duration};

use futures::future::join_all;
use lb_core::mantle::{
    GenesisTx as _,
    gas::GasCost,
    ops::channel::{ChannelId, deposit::DepositOp},
};
use lb_http_api_common::bodies::channel::ChannelDepositRequestBody;
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::{
    common::{sync::wait_for_validators_mode_and_height, time::max_block_propagation_time},
    nodes::{create_validator_config, validator::Validator},
    topology::configs::{
        create_general_configs, deployment::e2e_deployment_settings_with_genesis_tx,
    },
};
use serial_test::serial;
use tokio::time::sleep;

/// End-to-end test for the channel deposit flow:
///
/// 1. Spawn one validators that produce blocks.
/// 2. Call the POST `/channel/deposit` HTTP endpoint on the validator.
/// 4. Verify the API call succeeds (the endpoint returns 200).
/// 5. Wait for the transaction to be included in a block.
/// 6. Verify the funding key's wallet balanceting has decreased due to the
///    deposit.
#[tokio::test]
#[serial]
async fn channel_deposit() {
    // Spwan a validator
    let (configs, genesis_tx) = create_general_configs(1, None);
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx.clone());
    let configs: Vec<_> = configs
        .into_iter()
        .map(|c| {
            let mut config = create_validator_config(c, deployment_settings.clone());
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config.deployment.cryptarchia.security_param = NonZero::new(3).unwrap();
            config.deployment.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
            config
        })
        .collect();

    let validators: Vec<Validator> = join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to spawn validators");
    let validator = &validators[0];

    let target_height = 3;
    wait_for_validators_mode_and_height(
        &validators,
        lb_cryptarchia_engine::State::Online,
        target_height,
        max_block_propagation_time(
            target_height as u32,
            validators.len() as u64,
            &validator.config().deployment,
            3.0,
        ),
    )
    .await;

    // Record the funding key's balance before deposit
    let funding_pk = validator.config().user.cryptarchia.leader.wallet.funding_pk;
    let balance_before = get_wallet_balance(validator, funding_pk).await;
    println!("Wallet balance before deposit: {balance_before}");

    // Also, record the channel balance before deposit
    // We use the channel created by the genesis inscription for simplicity.
    let channel_id = genesis_tx.genesis_inscription().channel_id;
    let channel_balance_before = get_channel_balance(validator, channel_id).await;
    println!("Channel balance before deposit: {channel_balance_before}");

    // Trigger deposit via the HTTP API.
    let deposit_amount = 1;
    let body = ChannelDepositRequestBody {
        tip: None,
        deposit: DepositOp {
            channel_id,
            amount: deposit_amount,
            metadata: b"Mint 1 to Alice in Zone".to_vec(),
        },
        change_public_key: funding_pk,
        funding_public_keys: vec![funding_pk],
        max_tx_fee: GasCost::new(10),
    };
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{}/channel/deposit",
            validators[0].config().user.api.backend.listen_address
        ))
        .json(&body)
        .send()
        .await
        .expect("request should not fail");

    assert!(
        resp.status().is_success(),
        "request should succeed, got status: {} body: {}",
        resp.status(),
        resp.text().await.unwrap_or_default(),
    );

    // Wait for the deposit tx to be included (a few more blocks)
    let new_target_height = target_height + 5;
    wait_for_validators_mode_and_height(
        &validators,
        lb_cryptarchia_engine::State::Online,
        new_target_height,
        max_block_propagation_time(
            (new_target_height - target_height) as u32,
            validators.len() as u64,
            &validator.config().deployment,
            3.0,
        ),
    )
    .await;

    // Check the funding key's balance has decreased
    let balance_after = get_wallet_balance(validator, funding_pk).await;
    println!("Wallet balance after deposit: {balance_after}");
    assert_eq!(
        balance_after,
        balance_before - deposit_amount,
        "wallet balance should decrease after deposit: before={balance_before}, after={balance_after}, deposit_amount={deposit_amount}",
    );

    // Check the channel balance has increased
    let channel_balance_after = get_channel_balance(validator, channel_id).await;
    println!("Channel balance after deposit: {channel_balance_after}");
    assert_eq!(
        channel_balance_after,
        channel_balance_before + deposit_amount,
        "channel balance should decrease after deposit: before={balance_before}, after={balance_after}, deposit_amount={deposit_amount}",
    );
}

async fn get_wallet_balance(
    validator: &Validator,
    pk: lb_key_management_system_service::keys::ZkPublicKey,
) -> u64 {
    let pk_hex = hex::encode(lb_groth16::fr_to_bytes(&pk.into()));
    let url = format!(
        "http://{}/wallet/{}/balance",
        validator.config().user.api.backend.listen_address,
        pk_hex,
    );

    // Retry a few times - the wallet might not have processed the latest block yet
    for _ in 0..5 {
        let resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("balance request should not fail");

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await.unwrap();
            return body["balance"].as_u64().unwrap_or(0);
        }

        sleep(Duration::from_millis(500)).await;
    }

    panic!("Failed to get wallet balance after retries");
}

async fn get_channel_balance(validator: &Validator, channel_id: ChannelId) -> u64 {
    let url = format!(
        "http://{}/channel/{}",
        validator.config().user.api.backend.listen_address,
        channel_id,
    );

    // Retry a few times until the channel is created
    for _ in 0..5 {
        let resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("channel request should not fail");

        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await.unwrap();
            return body["balance"].as_u64().unwrap_or(0);
        }

        sleep(Duration::from_millis(500)).await;
    }

    panic!("Failed to get channel state after retries");
}
