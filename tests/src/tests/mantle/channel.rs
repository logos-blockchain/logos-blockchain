use std::{num::NonZero, path::PathBuf, time::Duration};

use futures::StreamExt as _;
use lb_common_http_client::ProcessedBlockEvent;
use lb_core::{
    events::{Event, Events, TxEvent, TxEventPayload},
    header::HeaderId,
    mantle::{
        GenesisTx as _, MantleTx, Note, NoteId, OpProof, SignedMantleTx, Transaction as _, TxHash,
        gas::GasCost,
        ledger::{Inputs, Outputs},
        ops::{
            Op,
            channel::{
                ChannelId, MsgId, deposit::DepositOp, inscribe::InscriptionOp,
                withdraw::ChannelWithdrawOp,
            },
        },
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_http_api_common::bodies::{
    channel::{ChannelDepositRequestBody, ChannelDepositResponseBody},
    wallet::balance::WalletBalanceResponseBody,
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_node::config::RunConfig;
use lb_testing_framework::{
    DeploymentBuilder, NodeHttpClient, TopologyConfig as TfTopologyConfig,
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::{
    common::manual_cluster::{
        ManualNodeLayout, api_url, get_wallet_balance, start_local_manual_cluster_with_layout,
        wait_for_nodes_height,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use serial_test::serial;
use testing_framework_core::scenario::DynError;
use tokio::time::{sleep, timeout};

/// End-to-end test for the channel deposit flow:
///
/// 1. Spawn validators that produce blocks.
/// 2. Call the POST `/channel/deposit` HTTP endpoint on one validator.
/// 3. Verify the API call succeeds.
/// 4. Wait for the deposit transaction to be included in a block.
/// 5. Verify the block containing the deposit tx exposes a matching `Deposit`
///    event via the `/cryptarchia/blocks/:id/events` endpoint.
/// 6. Verify the funding key's wallet balance decreases.
/// 7. Verify the channel balance increases.
#[tokio::test]
#[serial]
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
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
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
        .deployment()
        .config
        .genesis_block
        .as_ref()
        .expect("manual-cluster deployment should include genesis tx")
        .genesis_tx()
        .genesis_inscription()
        .channel_id;
    let channel_balance_before = get_channel_balance(&validator.client, channel_id).await;
    println!("Channel balance before deposit: {channel_balance_before}");

    // Subscribe before submitting so we can locate the block that includes the
    // deposit tx and then query its events via the HTTP API.
    let mut block_stream = validator.client.blocks_stream().await.unwrap();

    let (note_id, selected_deposit_amount) =
        get_wallet_note(&validator.client, funding_pk, deposit_amount).await;
    assert_eq!(selected_deposit_amount, deposit_amount);
    let deposit_op = DepositOp {
        channel_id,
        inputs: Inputs::new([note_id]),
        metadata: format!("Mint {deposit_amount} to Alice in Zone")
            .into_bytes()
            .try_into()
            .expect("Metadata too large for deposit op."),
    };
    let body = ChannelDepositRequestBody {
        tip: None,
        deposit: deposit_op.clone(),
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

    let deposit_tx_hash = response
        .json::<ChannelDepositResponseBody>()
        .await
        .expect("deposit response should be valid JSON")
        .hash;

    let deposit_block_id = timeout(Duration::from_mins(5), async {
        while let Some(event) = block_stream.next().await {
            if event
                .block
                .transactions
                .iter()
                .any(|tx| tx.hash() == deposit_tx_hash)
            {
                return event.block.header.id;
            }
        }
        panic!("blocks stream ended before deposit tx was observed");
    })
    .await
    .expect("timed out waiting for the deposit tx to be included in a block");

    let events = fetch_block_events(&validator.client, deposit_block_id).await;
    let (channel_id, amount, metadata) = events
        .iter()
        .find_map(|event| match event {
            Event::Tx(TxEvent {
                tx_hash,
                payload:
                    TxEventPayload::Deposit {
                        channel_id,
                        amount,
                        metadata,
                    },
                ..
            }) if tx_hash == &deposit_tx_hash => Some((*channel_id, *amount, metadata.clone())),
            _ => None,
        })
        .expect("block events should include the deposit event");
    assert_eq!(channel_id, deposit_op.channel_id);
    assert_eq!(amount, deposit_amount);
    assert_eq!(metadata, deposit_op.metadata);

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

/// End-to-end test for the channel withdraw wallet path:
///
/// 1. Spawn validators that produce blocks.
/// 2. Create a channel with a known signer.
/// 3. Deposit funds into that channel.
/// 4. Submit a signed channel withdraw transaction.
/// 5. Verify the recipient wallet balance increases.
/// 6. Verify the channel balance decreases.
#[tokio::test]
#[serial]
async fn channel_withdraw_updates_wallet_balance() {
    let deposit_amount = 5;
    let withdraw_amount = 2;
    let (wallet_config, funding_pk) = channel_deposit_wallet_config(deposit_amount, 100);
    let (_base, nodes) = start_local_manual_cluster_with_layout(
        "channel-withdraw-wallet-balance",
        "mantle-channel",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(2)
                .with_allow_multiple_genesis_tokens(true)
                .with_test_context(Some("channel_withdraw_updates_wallet_balance".to_owned())),
        )
        .with_wallet_config(wallet_config),
        2,
        ManualNodeLayout::SelectNodeSeed(0),
        |config| Ok::<_, DynError>(channel_test_config(config)),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
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

    let channel_id = ChannelId::from([42; 32]);
    let channel_signing_key = Ed25519Key::from_bytes(&[7; 32]);
    let signed_inscription_tx = signed_channel_inscription(channel_id, &channel_signing_key);

    let mut block_stream = validator.client.blocks_stream().await.unwrap();
    let inscription_tx_hash = signed_inscription_tx.hash();

    validator
        .client
        .submit_transaction(&signed_inscription_tx)
        .await
        .expect("inscription transaction should be submitted");

    wait_for_tx_inclusion(&mut block_stream, inscription_tx_hash, "inscription").await;

    let (deposit_op, deposit_tx_hash) =
        submit_channel_deposit(&validator.client, channel_id, funding_pk, deposit_amount).await;
    wait_for_tx_inclusion(&mut block_stream, deposit_tx_hash, "deposit").await;

    let balance_after_deposit =
        wait_for_wallet_balance(&validator.client, funding_pk, 100, Duration::from_mins(2)).await;
    let channel_balance_after_deposit = get_channel_balance(&validator.client, channel_id).await;
    assert_eq!(
        channel_balance_after_deposit, deposit_amount,
        "channel balance should increase after deposit: after={channel_balance_after_deposit}, deposit_amount={deposit_amount}",
    );

    let withdraw = ChannelWithdrawOp {
        channel_id,
        outputs: Outputs::new([Note::new(withdraw_amount, funding_pk)]),
        withdraw_nonce: 0,
    };
    let signed_withdraw_tx = signed_channel_withdraw(withdraw.clone(), &channel_signing_key);
    let withdraw_tx_hash = signed_withdraw_tx.hash();

    validator
        .client
        .submit_transaction(&signed_withdraw_tx)
        .await
        .expect("withdraw transaction should be submitted");

    wait_for_tx_inclusion(&mut block_stream, withdraw_tx_hash, "withdraw").await;

    let balance_after_withdraw = wait_for_wallet_balance(
        &validator.client,
        funding_pk,
        balance_after_deposit + withdraw_amount,
        Duration::from_mins(2),
    )
    .await;
    assert_eq!(
        balance_after_withdraw,
        balance_after_deposit + withdraw_amount,
        "wallet balance should increase after withdraw: before={balance_after_deposit}, after={balance_after_withdraw}, withdraw_amount={withdraw_amount}",
    );

    let channel_balance_after_withdraw = get_channel_balance(&validator.client, channel_id).await;
    assert_eq!(
        channel_balance_after_withdraw,
        channel_balance_after_deposit - withdraw_amount,
        "channel balance should decrease after withdraw: before={channel_balance_after_deposit}, after={channel_balance_after_withdraw}, withdraw_amount={withdraw_amount}",
    );

    assert_eq!(deposit_op.channel_id, withdraw.channel_id);
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

async fn submit_channel_deposit(
    node: &NodeHttpClient,
    channel_id: ChannelId,
    funding_pk: ZkPublicKey,
    deposit_amount: u64,
) -> (DepositOp, TxHash) {
    let (note_id, selected_deposit_amount) =
        get_wallet_note(node, funding_pk, deposit_amount).await;
    assert_eq!(selected_deposit_amount, deposit_amount);
    let deposit_op = DepositOp {
        channel_id,
        inputs: Inputs::new([note_id]),
        metadata: format!("Mint {deposit_amount} to Alice in Zone")
            .into_bytes()
            .try_into()
            .expect("Metadata too large for deposit op."),
    };
    let body = ChannelDepositRequestBody {
        tip: None,
        deposit: deposit_op.clone(),
        change_public_key: funding_pk,
        funding_public_keys: vec![funding_pk],
        max_tx_fee: GasCost::new(10),
    };
    let response = reqwest::Client::new()
        .post(api_url(node, "channel/deposit"))
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

    let deposit_tx_hash = response
        .json::<ChannelDepositResponseBody>()
        .await
        .expect("deposit response should be valid JSON")
        .hash;

    (deposit_op, deposit_tx_hash)
}

fn signed_channel_inscription(channel_id: ChannelId, signing_key: &Ed25519Key) -> SignedMantleTx {
    let inscription = InscriptionOp {
        channel_id,
        inscription: b"channel withdraw wallet balance test"
            .to_vec()
            .try_into()
            .expect("inscription payload should fit"),
        parent: MsgId::root(),
        signer: signing_key.public_key(),
    };
    let mantle_tx = MantleTx([Op::ChannelInscribe(inscription)].into());
    let tx_hash = mantle_tx.hash();
    let inscription_proof =
        OpProof::Ed25519Sig(signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref()));

    SignedMantleTx::new(mantle_tx, vec![inscription_proof])
        .expect("inscription transaction should be valid")
}

fn signed_channel_withdraw(
    withdraw: ChannelWithdrawOp,
    signing_key: &Ed25519Key,
) -> SignedMantleTx {
    let mantle_tx = MantleTx([Op::ChannelWithdraw(withdraw)].into());
    let tx_hash = mantle_tx.hash();
    let withdraw_proof = ChannelMultiSigProof::try_new(
        [IndexedSignature::new(
            0,
            signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref()),
        )]
        .into(),
    )
    .expect("withdraw proof should be valid");

    SignedMantleTx::new(
        mantle_tx,
        vec![OpProof::ChannelMultiSigProof(withdraw_proof)],
    )
    .expect("withdraw transaction should be valid")
}

async fn wait_for_tx_inclusion(
    block_stream: &mut (impl futures::Stream<Item = ProcessedBlockEvent> + Unpin),
    tx_hash: TxHash,
    label: &str,
) -> HeaderId {
    timeout(Duration::from_mins(5), async {
        while let Some(event) = block_stream.next().await {
            if event
                .block
                .transactions
                .iter()
                .any(|tx| tx.hash() == tx_hash)
            {
                return event.block.header.id;
            }
        }
        panic!("blocks stream ended before {label} tx was observed");
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for the {label} tx to be included in a block"))
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

async fn wait_for_wallet_balance(
    node: &NodeHttpClient,
    pk: ZkPublicKey,
    expected: u64,
    wait: Duration,
) -> u64 {
    let start = tokio::time::Instant::now();
    let mut last_balance = get_wallet_balance(node, pk).await;

    while start.elapsed() < wait {
        if last_balance == expected {
            return last_balance;
        }

        sleep(Duration::from_millis(500)).await;
        last_balance = get_wallet_balance(node, pk).await;
    }

    panic!("timed out waiting for wallet balance {expected}, last balance was {last_balance}");
}

async fn fetch_block_events(node: &NodeHttpClient, block_id: HeaderId) -> Events {
    let url = api_url(node, &format!("cryptarchia/blocks/{block_id}/events"));
    let response = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .expect("block events request should not fail");

    assert!(
        response.status().is_success(),
        "block events request should succeed, got status: {} body: {}",
        response.status(),
        response.text().await.unwrap_or_default(),
    );

    response
        .json::<Events>()
        .await
        .expect("block events response should be valid JSON")
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
