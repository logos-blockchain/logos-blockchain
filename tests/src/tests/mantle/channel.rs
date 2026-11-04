use std::{num::NonZero, path::PathBuf, time::Duration};

use futures::StreamExt as _;
use lb_common_http_client::ProcessedBlockEvent;
use lb_core::{
    events::{Event, Events, TxEvent, TxEventPayload},
    header::HeaderId,
    mantle::{
        Op, OpProof, SignedOps,
        gas::GasCost,
        ops::{
            ChannelInscribeProof,
            channel::{
                ChannelId, MsgId,
                deposit::Metadata,
                inscribe::{Inscription, InscriptionOp, MiniBlocks},
            },
        },
        traits::Hashable as _,
        transactions::{MantleTxBuilder, OpProofs, UNITS_PER_LGO, hash::TxHash},
    },
};
use lb_http_api_common::bodies::{
    channel::{ChannelDepositRequestBody, ChannelDepositResponseBody},
    wallet::fund::{WalletFundRequestBody, WalletFundResponseBody},
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

/// Generous cap on channel transaction fees at genesis gas prices; actual fees
/// are a few hundred gas units for these small transactions.
const MAX_CHANNEL_TX_FEE: u64 = 10_000 * UNITS_PER_LGO;
/// Covers all channel transaction fees in these tests with wide margin while
/// staying small relative to leader notes: genesis total stake is the sum of
/// all note values, so large fee notes dilute leader election.
const FEE_NOTE_AMOUNT: u64 = 50_000 * UNITS_PER_LGO;

/// End-to-end test for the channel deposit flow:
///
/// 1. Spawn validators that produce blocks.
/// 2. Call the POST `/channel/deposit` HTTP endpoint on one validator.
/// 3. Verify the API call succeeds.
/// 4. Wait for the deposit transaction to be included in a block.
/// 5. Verify the block containing the deposit tx exposes a matching `Deposit`
///    event via the `/cryptarchia/blocks/:id/events` endpoint.
/// 6. Verify the funding key's wallet balance decreases.
#[tokio::test]
#[serial]
async fn channel_deposit() {
    let deposit_amount = 1;
    let (wallet_config, funding_pk, _funding_account) =
        channel_deposit_wallet_config(deposit_amount, FEE_NOTE_AMOUNT);
    let (_base, nodes) = start_local_manual_cluster_with_layout(
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

    // Subscribe before submitting so we can locate the blocks that include the
    // inscription and deposit txs and then query their events via the HTTP API.
    let mut block_stream = validator.client.blocks_stream().await.unwrap();

    // The null channel of the genesis inscription has no note set, so the
    // deposit goes to a channel this test creates.
    let balance_before_channel = get_wallet_balance(&validator.client, funding_pk).await;
    let channel_id = create_channel(&validator.client, &mut block_stream, funding_pk).await;

    // The deposit is funded from the inscription's change, so the wallet has
    // to apply the block including the inscription first.
    let balance_before = timeout(Duration::from_mins(1), async {
        loop {
            let balance = get_wallet_balance(&validator.client, funding_pk).await;
            if balance != balance_before_channel {
                return balance;
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("timed out waiting for the wallet to apply the inscription");

    let deposit_metadata: Metadata = format!("Mint {deposit_amount} to Alice in Zone")
        .into_bytes()
        .try_into()
        .expect("Metadata too large for deposit op.");
    let body = ChannelDepositRequestBody {
        tip: None,
        channel_id,
        amount: deposit_amount,
        holder_public_key: funding_pk,
        metadata: deposit_metadata.clone(),
        change_public_key: funding_pk,
        funding_public_keys: vec![funding_pk],
        max_tx_fee: GasCost::new(MAX_CHANNEL_TX_FEE),
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

    let ChannelDepositResponseBody {
        hash: deposit_tx_hash,
        holder_note,
    } = response
        .json::<ChannelDepositResponseBody>()
        .await
        .expect("deposit response should be valid JSON");

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
    let (event_channel_id, metadata, outputs) = events
        .iter()
        .find_map(|event| match event {
            Event::Tx(TxEvent {
                tx_hash,
                payload:
                    TxEventPayload::Deposit {
                        channel_id,
                        metadata,
                        outputs,
                    },
                ..
            }) if tx_hash == &deposit_tx_hash => {
                Some((*channel_id, metadata.clone(), outputs.clone()))
            }
            _ => None,
        })
        .expect("block events should include the deposit event");
    assert_eq!(event_channel_id, channel_id);
    assert_eq!(metadata, deposit_metadata);
    // The deposit creates the holder note in the channel note set.
    assert_eq!(outputs.as_slice(), [holder_note.commitment()]);
    assert_eq!(holder_note.value, deposit_amount);
    assert_eq!(holder_note.public_key, funding_pk);

    let balance_after = get_wallet_balance(&validator.client, funding_pk).await;
    let spent = balance_before - balance_after;
    assert!(
        spent >= deposit_amount && spent <= deposit_amount + MAX_CHANNEL_TX_FEE,
        "wallet balance should decrease by deposit plus fee: before={balance_before}, after={balance_after}, deposit_amount={deposit_amount}",
    );
}

// Creates a channel with an inscription the node's wallet funds from
// `funding_pk`, and waits for its inclusion.
async fn create_channel(
    node: &NodeHttpClient,
    block_stream: &mut (impl futures::Stream<Item = ProcessedBlockEvent> + Unpin),
    funding_pk: ZkPublicKey,
) -> ChannelId {
    let signing_key = Ed25519Key::from_bytes(&[7u8; 32]);
    let channel_id = ChannelId::from(signing_key.public_key().to_bytes());
    let tx_builder = MantleTxBuilder::new()
        .push_op(Op::ChannelInscribe(InscriptionOp {
            channel_id,
            inscription: Inscription::try_from(b"channel deposit test".to_vec())
                .expect("inscription payload should fit"),
            parent: MsgId::root(),
            signer: signing_key.public_key().into_unverified(),
            mini_blocks: MiniBlocks::default(),
        }))
        .expect("inscription op should fit");
    let WalletFundResponseBody {
        funded_tx: ops,
        transfer_proof,
        ..
    } = node
        .fund_tx(WalletFundRequestBody {
            tip: None,
            tx_builder,
            change_public_key: funding_pk,
            funding_public_keys: vec![funding_pk],
            max_tx_fee: GasCost::new(MAX_CHANNEL_TX_FEE),
            priority_fee_percent: 0,
        })
        .await
        .expect("inscription should be funded");
    let tx_hash = ops.hash();
    let op_proofs = OpProofs::from([
        OpProof::ChannelInscribe(ChannelInscribeProof::without_steps(
            signing_key.sign_payload(tx_hash.as_signing_bytes()),
        )),
        transfer_proof.expect("funding at non-zero gas prices should add a transfer"),
    ]);
    let signed_tx = SignedOps::from_parts(ops, op_proofs).expect("funded tx should be valid");
    node.submit_transaction(&signed_tx)
        .await
        .expect("inscription should be submitted");
    wait_for_tx_inclusion(block_stream, tx_hash, "inscription").await;
    channel_id
}

/// End-to-end test for the channel withdraw wallet path:
///
/// 1. Spawn validators that produce blocks.
/// 2. Create a channel with a known signer.
/// 3. Deposit funds into that channel.
/// 4. Submit a signed channel withdraw transaction.
/// 5. Verify the recipient wallet balance increases.
/// 6. Verify the channel balance decreases.
// TODO: rebuild this scenario on `CHANNEL_TRANSFER` + `CHANNEL_WITHDRAW`. A
//  withdraw now only releases an existing channel note to the key it already
//  carries, so withdrawing part of a deposit first requires transferring a
//  channel note to split it. The channel balance assertions also need a source
//  other than `ChannelState`, which no longer carries a balance.
//  `cfg(any())` disables the test without deleting it.
#[cfg(any())]
#[tokio::test]
#[serial]
async fn channel_withdraw_updates_wallet_balance() {
    let deposit_amount = 5;
    let withdraw_amount = 2;
    let (wallet_config, funding_pk, funding_account) =
        channel_deposit_wallet_config(deposit_amount, FEE_NOTE_AMOUNT);
    let (base, nodes) = start_local_manual_cluster_with_layout(
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

    let genesis_utxos = genesis_wallet_utxos(&base.deployment().config);
    let initial_balance = get_wallet_balance(&validator.client, funding_pk).await;

    let channel_id = ChannelId::from([42; 32]);
    let channel_signing_key = Ed25519Key::from_bytes(&[7; 32]);
    let inscription_op = InscriptionOp {
        channel_id,
        inscription: b"channel withdraw wallet balance test"
            .to_vec()
            .try_into()
            .expect("inscription payload should fit"),
        parent: MsgId::root(),
        signer: channel_signing_key.public_key(),
    };
    let (signed_inscription_tx, inscription_fee) = funded_signed_tx(
        &validator.client,
        &genesis_utxos,
        &funding_account,
        HashMap::new(),
        Op::ChannelInscribe(inscription_op),
        |tx_hash| OpProof::Ed25519Sig(channel_signing_key.sign_payload(tx_hash.as_signing_bytes())),
    )
    .await;

    let mut block_stream = validator.client.blocks_stream().await.unwrap();
    let inscription_tx_hash = signed_inscription_tx.hash();

    validator
        .client
        .submit_transaction(&signed_inscription_tx)
        .await
        .expect("inscription transaction should be submitted");

    wait_for_tx_inclusion(&mut block_stream, inscription_tx_hash, "inscription").await;

    let balance_after_inscription = wait_for_wallet_balance(
        &validator.client,
        funding_pk,
        initial_balance - inscription_fee,
        Duration::from_mins(2),
    )
    .await;

    let (deposit_op, deposit_tx_hash) =
        submit_channel_deposit(&validator.client, channel_id, funding_pk, deposit_amount).await;
    wait_for_tx_inclusion(&mut block_stream, deposit_tx_hash, "deposit").await;

    // The deposit is funded server-side, so its exact fee is unknown here; it
    // may even be zero once gas prices decay, so wait only for the deposit
    // amount to leave the wallet.
    let balance_after_deposit = wait_for_wallet_balance_at_most(
        &validator.client,
        funding_pk,
        balance_after_inscription - deposit_amount,
        Duration::from_mins(2),
    )
    .await;
    assert!(
        balance_after_deposit >= balance_after_inscription - deposit_amount - MAX_CHANNEL_TX_FEE,
        "deposit fee should stay within the max fee bound: after_inscription={balance_after_inscription}, after_deposit={balance_after_deposit}",
    );
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
    let (signed_withdraw_tx, withdraw_fee) = funded_signed_tx(
        &validator.client,
        &genesis_utxos,
        &funding_account,
        HashMap::from([(channel_id, 1)]),
        Op::ChannelWithdraw(withdraw.clone()),
        |tx_hash| {
            OpProof::ChannelMultiSigProof(ChannelMultiSigProof::new(IndexedSignatures::from((
                0,
                channel_signing_key.sign_payload(tx_hash.as_signing_bytes()),
            ))))
        },
    )
    .await;
    let withdraw_tx_hash = signed_withdraw_tx.hash();

    validator
        .client
        .submit_transaction(&signed_withdraw_tx)
        .await
        .expect("withdraw transaction should be submitted");

    wait_for_tx_inclusion(&mut block_stream, withdraw_tx_hash, "withdraw").await;

    let expected_after_withdraw = balance_after_deposit + withdraw_amount - withdraw_fee;
    let balance_after_withdraw = wait_for_wallet_balance(
        &validator.client,
        funding_pk,
        expected_after_withdraw,
        Duration::from_mins(2),
    )
    .await;
    assert_eq!(
        balance_after_withdraw, expected_after_withdraw,
        "wallet balance should increase by withdraw minus fee: before={balance_after_deposit}, after={balance_after_withdraw}, withdraw_amount={withdraw_amount}, withdraw_fee={withdraw_fee}",
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
) -> (WalletConfig, ZkPublicKey, WalletAccount) {
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
    let funding_account = deposit_note.clone();

    (
        WalletConfig::new(vec![deposit_note, fee_note]),
        funding_pk,
        funding_account,
    )
}

fn channel_test_config(mut config: RunConfig) -> RunConfig {
    config
        .deployment
        .update_genesis_era_parameters(|parameters| {
            parameters.time.slot_duration_in_seconds = 1.try_into().unwrap();
            parameters.cryptarchia.security_param = NonZero::new(3).unwrap();
            parameters.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
        });
    config
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

#[expect(
    dead_code,
    reason = "Used by `channel_withdraw_updates_wallet_balance`, restore when wallet tracks channel notes"
)]
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

#[expect(
    dead_code,
    reason = "Used by `channel_withdraw_updates_wallet_balance`, restore when wallet tracks channel notes"
)]
async fn wait_for_wallet_balance_at_most(
    node: &NodeHttpClient,
    pk: ZkPublicKey,
    threshold: u64,
    wait: Duration,
) -> u64 {
    let start = tokio::time::Instant::now();
    let mut last_balance = get_wallet_balance(node, pk).await;

    while start.elapsed() < wait {
        if last_balance <= threshold {
            return last_balance;
        }

        sleep(Duration::from_millis(500)).await;
        last_balance = get_wallet_balance(node, pk).await;
    }

    panic!(
        "timed out waiting for wallet balance at most {threshold}, last balance was {last_balance}"
    );
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

#[expect(
    dead_code,
    reason = "Restored with the channel balance assertions once channel notes are tracked."
)]
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
