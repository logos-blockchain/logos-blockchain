use std::{num::NonZero, path::PathBuf, time::Duration};

use futures::StreamExt as _;
use lb_common_http_client::{CommonHttpClient, ProcessedBlockEvent};
use lb_config::consensus::UNITS_PER_LGO;
use lb_core::{
    events::{Event, Events, TxEvent, TxEventPayload},
    header::HeaderId,
    mantle::{
        Note, Op, OpProof, SignedOps,
        gas::GasCost,
        notes::NoteCommitment,
        ops::{
            ChannelInscribeProof, OpRef,
            channel::{
                ChannelId, MsgId,
                deposit::Metadata,
                inscribe::{Inscription, InscriptionOp, MiniBlock, MiniBlocks},
            },
        },
        traits::Hashable as _,
        transactions::{MantleTxBuilder, OpProofs, hash::TxHash},
    },
};
use lb_http_api_common::{
    bodies::{
        channel::{
            ChannelDepositRequestBody, ChannelPayment, ChannelPrepareDepositResponseBody,
            ChannelStepRequestBody, ChannelStepResponseBody, ChannelWithdrawRequestBody,
            ChannelWithdrawResponseBody,
        },
        wallet::{
            fund::{WalletFundRequestBody, WalletFundResponseBody},
            transfer_funds::{WalletPrepareTransferResponseBody, WalletTransferFundsRequestBody},
        },
    },
    paths,
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
        wait_for_nodes_height, wait_for_spendable,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use reqwest::StatusCode;
use serde_json::Value;
use serial_test::serial;
use testing_framework_core::scenario::DynError;
use tokio::time::{sleep, timeout};

/// Generous cap on channel transaction fees at genesis gas prices; actual fees
/// are a few hundred gas units for these small transactions.
const MAX_CHANNEL_TX_FEE: u64 = 20_000 * UNITS_PER_LGO;
/// Covers all channel transaction fees in these tests with wide margin while
/// staying small relative to leader notes: genesis total stake is the sum of
/// all note values, so large fee notes dilute leader election.
const FEE_NOTE_AMOUNT: u64 = 50_000 * UNITS_PER_LGO;

/// End-to-end test for the channel deposit flow:
///
/// 1. Spawn validators that produce blocks.
/// 2. Prepare the deposit through POST `/channel/prepare-deposit` on one
///    validator.
/// 3. Verify the API call succeeds.
/// 4. Submit the returned transaction and wait for its inclusion in a block.
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
    let (channel_id, _, inscription_change) =
        create_channel(&validator.client, &mut block_stream, funding_pk).await;
    let tip = wait_for_spendable_note(&validator.client, funding_pk, inscription_change).await;
    let balance_before = get_wallet_balance(&validator.client, funding_pk).await;

    let deposit_metadata =
        Metadata::new_unchecked(format!("Mint {deposit_amount} to Alice in Zone").into_bytes());
    let body = ChannelDepositRequestBody {
        tip: Some(tip),
        channel_id,
        amount: deposit_amount,
        holder_public_key: funding_pk,
        metadata: deposit_metadata.clone(),
        change_public_key: funding_pk,
        funding_public_keys: vec![funding_pk],
        max_tx_fee: GasCost::new(MAX_CHANNEL_TX_FEE),
    };
    let response = reqwest::Client::new()
        .post(api_url(&validator.client, paths::CHANNEL_PREPARE_DEPOSIT))
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

    let ChannelPrepareDepositResponseBody {
        hash: deposit_tx_hash,
        transaction,
        holder_note,
    } = response
        .json::<ChannelPrepareDepositResponseBody>()
        .await
        .expect("deposit response should be valid JSON");
    validator
        .client
        .submit_transaction(&transaction)
        .await
        .expect("the prepared deposit should be submitted");

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

    // The wallet applies the block of the deposit after the block stream
    // reports it.
    let balance_after = timeout(Duration::from_mins(1), async {
        loop {
            let balance = get_wallet_balance(&validator.client, funding_pk).await;
            if balance != balance_before {
                return balance;
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("timed out waiting for the wallet to apply the deposit");
    let spent = balance_before - balance_after;
    assert!(
        spent >= deposit_amount && spent <= deposit_amount + MAX_CHANNEL_TX_FEE,
        "wallet balance should decrease by deposit plus fee: before={balance_before}, after={balance_after}, deposit_amount={deposit_amount}",
    );
}

/// A caller can keep a prepared transaction's complete JSON reply and safely
/// retry its exact transaction after losing the submission response and after
/// the node restarts. The caller data stays in memory in this test.
#[tokio::test]
#[serial]
async fn prepared_transactions_can_be_submitted_and_retried_after_restart() {
    let (wallet_config, funding_pk, _) = channel_deposit_wallet_config(1, FEE_NOTE_AMOUNT);
    let (base, nodes) = start_local_manual_cluster_with_layout(
        "prepare-transactions",
        "mantle-prepare-transactions",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(2)
                .with_allow_multiple_genesis_tokens(true)
                .with_test_context(Some(
                    "prepared_transactions_can_be_submitted_and_retried_after_restart".to_owned(),
                )),
        )
        .with_wallet_config(wallet_config),
        2,
        ManualNodeLayout::SelectNodeSeed(0),
        |config| Ok::<_, DynError>(channel_test_config(config)),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    )
    .await;
    let validator = &nodes[0];
    let http = CommonHttpClient::new(None);
    wait_for_nodes_height(
        &nodes.iter().map(|node| &node.client).collect::<Vec<_>>(),
        3,
        Duration::from_mins(5),
    )
    .await;

    let mut blocks = validator.client.blocks_stream().await.unwrap();
    let (channel_id, _, inscription_change) =
        create_channel(&validator.client, &mut blocks, funding_pk).await;
    let deposit_tip =
        wait_for_spendable_note(&validator.client, funding_pk, inscription_change).await;

    // The holder key is outside this node's wallet. Keep the whole prepare
    // reply before posting only its transaction value to the existing endpoint.
    let external_pk = WalletAccount::deterministic(100, 1, false)
        .expect("external key should be valid")
        .public_key();
    let prepared_deposit = http
        .prepare_channel_deposit(
            api_url(&validator.client, ""),
            ChannelDepositRequestBody {
                tip: Some(deposit_tip),
                channel_id,
                amount: 1,
                holder_public_key: external_pk,
                metadata: Metadata::new_unchecked(b"prepared deposit".to_vec()),
                change_public_key: funding_pk,
                funding_public_keys: vec![funding_pk],
                max_tx_fee: GasCost::new(MAX_CHANNEL_TX_FEE),
            },
        )
        .await
        .expect("deposit should be prepared");
    let saved_deposit =
        serde_json::to_string(&prepared_deposit).expect("prepared deposit reply should serialize");
    let deposit: ChannelPrepareDepositResponseBody =
        serde_json::from_str(&saved_deposit).expect("saved deposit reply should decode");
    let deposit_json: Value = serde_json::from_str(&saved_deposit).unwrap();
    assert_eq!(deposit.hash, deposit.transaction.hash());
    assert_eq!(deposit.holder_note.public_key, external_pk);
    assert_eq!(deposit.holder_note.value, 1);
    assert_not_published(&validator.client, deposit.hash).await;
    discard_submission_response(&validator.client, &deposit_json["transaction"]).await;
    // Retry the same transaction immediately after losing the first reply.
    discard_submission_response(&validator.client, &deposit_json["transaction"]).await;

    let deposit_block = wait_for_tx_inclusion(&mut blocks, deposit.hash, "prepared deposit").await;
    let events = fetch_block_events(&validator.client, deposit_block).await;
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Tx(TxEvent {
            tx_hash,
            payload: TxEventPayload::Deposit { channel_id: event_channel, outputs, .. },
            ..
        }) if tx_hash == &deposit.hash
            && event_channel == &channel_id
            && outputs.as_slice() == [deposit.holder_note.commitment()]
    )));
    let Some(OpRef::Transfer(funding)) = deposit.transaction.op_refs_iter().last() else {
        panic!("deposit should end with its fee transfer");
    };
    let deposit_change = *funding
        .outputs
        .last()
        .expect("fee transfer should return change");
    let transfer_tip = wait_for_spendable_note(&validator.client, funding_pk, deposit_change).await;

    // The transfer uses the same prepare/save/submit flow for a ledger note.
    let prepared_transfer = http
        .prepare_transfer_funds(
            api_url(&validator.client, ""),
            WalletTransferFundsRequestBody {
                tip: Some(transfer_tip),
                change_public_key: funding_pk,
                funding_public_keys: vec![funding_pk],
                recipient_public_key: external_pk,
                amount: 1,
            },
        )
        .await
        .expect("transfer should be prepared");
    let saved_transfer = serde_json::to_string(&prepared_transfer)
        .expect("prepared transfer reply should serialize");
    let transfer: WalletPrepareTransferResponseBody =
        serde_json::from_str(&saved_transfer).expect("saved transfer reply should decode");
    let transfer_json: Value = serde_json::from_str(&saved_transfer).unwrap();
    assert_eq!(transfer.hash, transfer.transaction.hash());
    assert_eq!(transfer.recipient_note.public_key, external_pk);
    assert_eq!(transfer.recipient_note.value, 1);
    assert_not_published(&validator.client, transfer.hash).await;
    discard_submission_response(&validator.client, &transfer_json["transaction"]).await;
    // Retry the same transaction immediately after losing the first reply.
    discard_submission_response(&validator.client, &transfer_json["transaction"]).await;

    let transfer_block_id =
        wait_for_tx_inclusion(&mut blocks, transfer.hash, "prepared transfer").await;
    let transfer_block = validator
        .client
        .block(&transfer_block_id)
        .await
        .unwrap()
        .unwrap();
    let transaction = transfer_block
        .transactions
        .iter()
        .find(|tx| tx.hash() == transfer.hash)
        .expect("included block should contain the saved transfer");
    assert!(transaction.op_refs_iter().any(|op| matches!(
        op,
        OpRef::Transfer(op) if op.outputs.contains(&transfer.recipient_note.commitment())
    )));
    let Some(OpRef::Transfer(funding)) = transfer.transaction.op_refs_iter().last() else {
        panic!("transfer should end with its fee transfer");
    };
    let transfer_change = *funding
        .outputs
        .last()
        .expect("fee transfer should return change");
    wait_for_spendable_note(&validator.client, funding_pk, transfer_change).await;
    let confirmed_balance = get_wallet_balance(&validator.client, funding_pk).await;

    // The node reopens its persisted data; the caller still has its JSON.
    let cluster = base.cluster();
    cluster
        .restart_node(&validator.name)
        .await
        .expect("node should restart");
    cluster
        .wait_node_ready(&validator.name)
        .await
        .expect("restarted node should become ready");
    let restarted = cluster
        .node_client(&validator.name)
        .expect("restarted client should exist");
    wait_for_spendable_note(&restarted, funding_pk, transfer_change).await;
    assert_eq!(
        get_wallet_balance(&restarted, funding_pk).await,
        confirmed_balance
    );
    let height = restarted
        .consensus_info()
        .await
        .unwrap()
        .cryptarchia_info
        .height;
    for saved in [&deposit_json, &transfer_json] {
        discard_submission_response(&restarted, &saved["transaction"]).await;
    }
    wait_for_nodes_height(&[&restarted], height + 3, Duration::from_mins(2)).await;
    assert_eq!(
        get_wallet_balance(&restarted, funding_pk).await,
        confirmed_balance
    );
}

async fn discard_submission_response(node: &NodeHttpClient, transaction: &Value) {
    let response = reqwest::Client::new()
        .post(api_url(node, paths::MEMPOOL_ADD_TX))
        .json(transaction)
        .send()
        .await
        .expect("submission should reach the node");
    assert!(
        response.status().is_success(),
        "submission failed: {} {}",
        response.status(),
        response.text().await.unwrap_or_default()
    );
    // Drop the successful response without reading it.
}

async fn assert_not_published(node: &NodeHttpClient, hash: TxHash) {
    assert!(!node.test_mempool_view().await.unwrap().contains(&hash));
    // Accepted transactions are stored before submission returns. This also
    // catches one already included and therefore absent from the pending pool.
    let path = paths::TRANSACTION.replace("{id}", &hash.to_string());
    let response = reqwest::Client::new()
        .get(api_url(node, &path))
        .send()
        .await
        .expect("transaction lookup should reach the node");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// End-to-end test for a user step:
///
/// 1. Create a channel and deposit into it, to a key of the node's wallet.
/// 2. Call the POST `/channel/step` HTTP endpoint for a user step paying part
///    of the deposit, once the deposited note is deep enough to be spent.
/// 3. Inscribe the step with its proof and wait for its inclusion.
/// 4. Call the POST `/channel/withdraw` HTTP endpoint for the note the step
///    paid, once that note is deep enough to be spent.
/// 5. Verify the withdrawing note is neither withdrawn again nor spent by a
///    step, and a program deposit is proven from the note left.
/// 6. Verify the ledger balance of the key rises by the note's value once the
///    withdrawal is released.
#[tokio::test]
#[serial]
async fn channel_note_round_trip() {
    let deposit_amount = 9;
    let (wallet_config, funding_pk, _funding_account) =
        channel_deposit_wallet_config(deposit_amount, FEE_NOTE_AMOUNT);
    let (_base, nodes) = start_local_manual_cluster_with_layout(
        "channel-note-round-trip",
        "mantle-channel",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(2)
                .with_allow_multiple_genesis_tokens(true)
                .with_test_context(Some("channel_note_round_trip".to_owned())),
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

    let mut block_stream = validator.client.blocks_stream().await.unwrap();
    let balance_before_channel = get_wallet_balance(&validator.client, funding_pk).await;
    let (channel_id, parent, _) =
        create_channel(&validator.client, &mut block_stream, funding_pk).await;
    timeout(Duration::from_mins(1), async {
        while get_wallet_balance(&validator.client, funding_pk).await == balance_before_channel {
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("timed out waiting for the wallet to apply the inscription");
    wait_for_spendable(&validator.client, funding_pk, Duration::from_mins(2)).await;

    let response = reqwest::Client::new()
        .post(api_url(&validator.client, paths::CHANNEL_PREPARE_DEPOSIT))
        .json(&ChannelDepositRequestBody {
            tip: None,
            channel_id,
            amount: deposit_amount,
            holder_public_key: funding_pk,
            metadata: Metadata::new_unchecked(b"deposit".to_vec()),
            change_public_key: funding_pk,
            funding_public_keys: vec![funding_pk],
            max_tx_fee: GasCost::new(MAX_CHANNEL_TX_FEE),
        })
        .send()
        .await
        .expect("request should not fail");
    assert!(response.status().is_success(), "deposit should succeed");
    let ChannelPrepareDepositResponseBody {
        hash: deposit_tx_hash,
        transaction,
        ..
    } = response
        .json::<ChannelPrepareDepositResponseBody>()
        .await
        .expect("deposit response");
    validator
        .client
        .submit_transaction(&transaction)
        .await
        .expect("the prepared deposit should be submitted");
    wait_for_tx_inclusion(&mut block_stream, deposit_tx_hash, "deposit").await;

    // The deposited note is spent once the wallet adopted it and its block is
    // deep enough.
    let first_paid = 4;
    let ChannelStepResponseBody {
        step: first_step,
        proof: first_proof,
        outputs: first_outputs,
        ..
    } = channel_user_step(
        &validator.client,
        channel_id,
        funding_pk,
        vec![],
        first_paid,
    )
    .await;
    assert_eq!(
        first_outputs
            .iter()
            .map(|note| note.value)
            .collect::<Vec<_>>(),
        vec![first_paid, deposit_amount - first_paid]
    );

    // A step of the next mini-block is proven behind the notes the first one
    // creates, and spends them before they are on chain.
    let paid = 6;
    let ChannelStepResponseBody {
        step,
        proof,
        outputs,
        ..
    } = channel_user_step(
        &validator.client,
        channel_id,
        funding_pk,
        first_outputs.iter().map(Note::commitment).collect(),
        paid,
    )
    .await;
    assert_eq!(
        step.inputs.len(),
        2,
        "the second step should spend both notes of the first"
    );
    assert_eq!(
        outputs.iter().map(|note| note.value).collect::<Vec<_>>(),
        vec![paid, deposit_amount - paid]
    );

    let signing_key = Ed25519Key::from_bytes(&[7u8; 32]);
    let tx_builder = MantleTxBuilder::new()
        .push_op(Op::ChannelInscribe(InscriptionOp {
            channel_id,
            inscription: Inscription::new_unchecked(b"zone block".to_vec()),
            parent,
            signer: signing_key.public_key().into_unverified(),
            mini_blocks: MiniBlocks::from([
                MiniBlock::try_from(vec![first_step]).expect("one step fits a mini-block"),
                MiniBlock::try_from(vec![step]).expect("one step fits a mini-block"),
            ]),
        }))
        .expect("inscription op should fit");
    let WalletFundResponseBody {
        funded_tx: ops,
        transfer_proof,
        ..
    } = validator
        .client
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
        OpProof::ChannelInscribe(ChannelInscribeProof::new(
            signing_key.sign_payload(tx_hash.as_signing_bytes()),
            vec![first_proof, proof],
        )),
        transfer_proof.expect("funding at non-zero gas prices should add a transfer"),
    ]);
    let signed_tx = SignedOps::from_parts(ops, op_proofs).expect("funded tx should be valid");
    validator
        .client
        .submit_transaction(&signed_tx)
        .await
        .expect("inscription with a user step should be submitted");
    wait_for_tx_inclusion(&mut block_stream, tx_hash, "user step inscription").await;

    // The paid note is withdrawn once the wallet adopted it and its block is
    // deep enough.
    let balance_before_withdraw = get_wallet_balance(&validator.client, funding_pk).await;
    let withdraw_request = || ChannelWithdrawRequestBody {
        tip: None,
        channel_id,
        notes: vec![outputs[0].commitment()],
        change_public_key: funding_pk,
        funding_public_keys: vec![funding_pk],
        max_tx_fee: GasCost::new(MAX_CHANNEL_TX_FEE),
    };
    let ChannelWithdrawResponseBody {
        hash: withdraw_tx_hash,
        outputs: released,
    } = retry_until_spendable("withdraw", || {
        validator.client.channel_withdraw(withdraw_request())
    })
    .await;
    assert_eq!(
        released
            .iter()
            .map(|note| (note.value, note.public_key))
            .collect::<Vec<_>>(),
        vec![(paid, funding_pk)]
    );
    wait_for_tx_inclusion(&mut block_stream, withdraw_tx_hash, "withdraw").await;

    // The withdrawing note is left to its withdrawal: it is not withdrawn
    // again, and a step is built from the change alone.
    let change = deposit_amount - paid;
    let error = validator
        .client
        .channel_withdraw(withdraw_request())
        .await
        .err()
        .expect("a withdrawing note should not be withdrawn again");
    assert!(error.to_string().contains("in flight"), "{error}");
    let error = validator
        .client
        .channel_step(user_step_request(
            channel_id,
            funding_pk,
            vec![],
            change + 1,
        ))
        .await
        .err()
        .expect("a step should not spend a withdrawing note");
    assert!(
        error.to_string().contains(&format!("available={change}")),
        "{error}"
    );
    let error = validator
        .client
        .channel_step(user_step_request(channel_id, funding_pk, vec![], 0))
        .await
        .err()
        .expect("a step moving no value should be refused");
    assert!(error.to_string().contains("400"), "{error}");

    // The withdrawal costs its fee at inclusion and pays the note at release.
    let balance_after_fee = timeout(Duration::from_secs(10), async {
        loop {
            let balance = get_wallet_balance(&validator.client, funding_pk).await;
            if balance != balance_before_withdraw {
                return balance;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("timed out waiting for the wallet to apply the withdrawal");
    assert!(balance_after_fee < balance_before_withdraw);
    wait_for_wallet_balance(
        &validator.client,
        funding_pk,
        balance_after_fee + paid,
        Duration::from_mins(3),
    )
    .await;
}

// A user step paying `paid` to `funding_pk` from its channel notes, behind the
// `appended` commitments.
async fn channel_user_step(
    node: &NodeHttpClient,
    channel_id: ChannelId,
    funding_pk: ZkPublicKey,
    appended: Vec<NoteCommitment>,
    paid: u64,
) -> ChannelStepResponseBody {
    retry_until_spendable("user step", || {
        node.channel_step(user_step_request(
            channel_id,
            funding_pk,
            appended.clone(),
            paid,
        ))
    })
    .await
}

fn user_step_request(
    channel_id: ChannelId,
    funding_pk: ZkPublicKey,
    appended: Vec<NoteCommitment>,
    paid: u64,
) -> ChannelStepRequestBody {
    ChannelStepRequestBody {
        tip: None,
        channel_id,
        appended,
        payments: vec![ChannelPayment {
            public_key: funding_pk,
            value: paid,
        }],
        inscription: Inscription::new_unchecked(b"user step".to_vec()),
        change_public_key: funding_pk,
        holder_public_keys: vec![funding_pk],
    }
}

// Retries `request` until the wallet can spend the channel notes it needs:
// one is refused until the wallet adopted it and its block is deep enough.
async fn retry_until_spendable<Response, Request>(
    label: &str,
    request: impl Fn() -> Request + Send + Sync,
) -> Response
where
    Response: Send,
    Request: Future<Output = Result<Response, lb_common_http_client::Error>> + Send,
{
    let mut last_error = None;
    let response = timeout(Duration::from_mins(2), async {
        loop {
            match request().await {
                Ok(response) => return response,
                Err(error) => last_error = Some(error),
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    response.unwrap_or_else(|_| {
        panic!("timed out waiting for the {label} to succeed, last error: {last_error:?}")
    })
}

// Creates a channel with an inscription the node's wallet funds from
// `funding_pk` and returns the channel, its tip, and the funding change.
async fn create_channel(
    node: &NodeHttpClient,
    block_stream: &mut (impl futures::Stream<Item = ProcessedBlockEvent> + Unpin),
    funding_pk: ZkPublicKey,
) -> (ChannelId, MsgId, NoteCommitment) {
    let signing_key = Ed25519Key::from_bytes(&[7u8; 32]);
    let channel_id = ChannelId::from(signing_key.public_key().to_bytes());
    let inscription = InscriptionOp {
        channel_id,
        inscription: Inscription::new_unchecked(b"channel deposit test".to_vec()),
        parent: MsgId::root(),
        signer: signing_key.public_key().into_unverified(),
        mini_blocks: MiniBlocks::default(),
    };
    let tip = inscription.id();
    let tx_builder = MantleTxBuilder::new()
        .push_op(Op::ChannelInscribe(inscription))
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
    let Some(Op::Transfer(funding)) = ops.last() else {
        panic!("inscription should end with its fee transfer");
    };
    let change = *funding
        .outputs
        .last()
        .expect("fee transfer should return change");
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
    (channel_id, tip, change)
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
                NonNegativeRatio::new(1, 2.try_into().unwrap())
                    .try_into()
                    .unwrap();
            // The widest uncle reference window k allows, ⌊0.6k⌋.
            parameters.cryptarchia.uncle_reference_window_in_block = 1;
            // Short enough for a test to see a withdrawal released.
            parameters.cryptarchia.channel_withdraw_delay = NonZero::new(30).unwrap();
        })
        .expect("The changed deployment must be valid.");
    config
}

// Wait for this transaction's change, rather than any mature note. Pin the
// next preparation to the same state so a competing tip cannot hide its fee
// source.
async fn wait_for_spendable_note(
    node: &NodeHttpClient,
    public_key: ZkPublicKey,
    commitment: NoteCommitment,
) -> HeaderId {
    let http = CommonHttpClient::new(None);
    timeout(Duration::from_mins(2), async {
        loop {
            if let Ok(balance) = http
                .get_wallet_balance(api_url(node, ""), public_key, None)
                .await
                && balance.notes.contains_key(&commitment)
                && !balance.pending_notes.contains(&commitment)
            {
                return balance.tip;
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("the transaction's change should become spendable")
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
