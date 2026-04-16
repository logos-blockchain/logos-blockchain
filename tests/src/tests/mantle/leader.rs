use std::{num::NonZero, time::Duration};

use futures::future::join_all;
use lb_node::config::cryptarchia::deployment::EpochConfig;
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::{
    common::{
        sync::{wait_for_validators_mode_and_height, wait_for_validators_mode_and_slot},
        time::max_block_propagation_time,
    },
    nodes::{create_validator_config, validator::Validator},
    topology::configs::{
        create_general_configs, deployment::e2e_deployment_settings_with_genesis_tx,
    },
};
use serial_test::serial;
use tokio::time::sleep;

/// End-to-end test for the leader claim flow:
///
/// 1. Spawn one validators that produce blocks (generating vouchers).
/// 2. Wait for enough blocks so that at least one epoch transition occurs,
///    flushing pending vouchers into the MMR and distributing merkle paths to
///    wallets.
/// 3. Call the POST `/leader/claim` HTTP endpoint on the validator.
/// 4. Verify the claim succeeds (the endpoint returns 200).
/// 5. Wait for the claim transaction to be included in a block.
/// 6. Verify the funding key's wallet balance has increased due to rewards.
#[tokio::test]
#[serial]
async fn leader_claim() {
    // Spwan a validator with a short epoch length
    let (configs, genesis_tx) = create_general_configs(1, Some("leader_claim"));
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
    let configs: Vec<_> = configs
        .into_iter()
        .map(|c| {
            let mut config = create_validator_config(c, deployment_settings.clone());
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
        })
        .collect();

    let validators: Vec<Validator> = join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to spawn validators");

    // Wait for 2 epoch transitions:
    // epoch 0→1 (vouchers become pending), epoch 1→2 (vouchers flushed into MMR).
    let validator = &validators[0];
    let target_slot = 2 * validator.config().deployment.cryptarchia.slots_per_epoch();
    println!(
        "target_slot:{target_slot}, deployment.cryptarchia:{:?}",
        validator.config().deployment.cryptarchia
    );
    wait_for_validators_mode_and_slot(
        &validators,
        lb_cryptarchia_engine::State::Online,
        target_slot.into(),
        Duration::from_secs(300),
    )
    .await;

    // Record the funding key's balance before claiming
    let funding_pk = validator.config().user.cryptarchia.leader.wallet.funding_pk;
    let balance_before = get_wallet_balance(validator, funding_pk).await;
    println!("Balance before claim: {balance_before}");

    // Trigger leader claim via the HTTP API
    let claim_response = reqwest::Client::new()
        .post(format!(
            "http://{}/leader/claim",
            validators[0].config().user.api.backend.listen_address
        ))
        .send()
        .await
        .expect("leader claim request should not fail");

    assert!(
        claim_response.status().is_success(),
        "leader claim should succeed, got status: {} body: {}",
        claim_response.status(),
        claim_response.text().await.unwrap_or_default(),
    );

    // Wait for the claim tx to be included (a few more blocks)
    let tip_height = validator.consensus_info(false).await.height;
    let target_height = tip_height + 5;
    wait_for_validators_mode_and_height(
        &validators,
        lb_cryptarchia_engine::State::Online,
        target_height,
        max_block_propagation_time(
            5,
            validators.len() as u64,
            &validator.config().deployment,
            3.0,
        ),
    )
    .await;

    // Check the funding key's balance has increased
    let balance_after = get_wallet_balance(validator, funding_pk).await;
    println!("Balance after claim: {balance_after}");

    assert!(
        balance_after > balance_before,
        "balance should increase after claiming rewards: before={balance_before}, after={balance_after}",
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

    // Retry a few times — the wallet might not have processed the latest block yet
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
