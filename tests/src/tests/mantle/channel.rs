use std::{num::NonZero, path::PathBuf, time::Duration};

use futures::StreamExt as _;
use lb_common_http_client::ProcessedBlockEvent;
use lb_config::consensus::UNITS_PER_LGO;
use lb_core::{
    events::{Event, Events, TxEvent, TxEventPayload},
    header::HeaderId,
    mantle::{
        Note, Op, OpProof, SignedOps,
        batch::test_utils::batch_verify,
        gas::GasCost,
        notes::NoteCommitment,
        ops::{
            ChannelInscribeProof, OpId as _, ProgramProof, ProgramStepProof, StepProof,
            channel::{
                ChannelId, MsgId,
                deposit::Metadata,
                inscribe::{Inscription, InscriptionOp, MiniBlock, MiniBlocks, Step},
                program_step::{program_deposit_id, program_deposits_hash},
            },
            zk_transfer::defer_zk_transfer,
        },
        programs::ProgramVerificationKey,
        traits::Hashable as _,
        transactions::{MantleTxBuilder, OpProofs, hash::TxHash},
    },
};
use lb_groth16::{
    CompressedGroth16Proof, Fr, fr_from_mod_bytes,
    test_utils::{three_input_prove, three_input_setup},
};
use lb_http_api_common::bodies::{
    channel::{
        ChannelDepositRequestBody, ChannelDepositResponseBody, ChannelPayment,
        ChannelStepRequestBody, ChannelStepResponseBody, ChannelWithdrawRequestBody,
        ChannelWithdrawResponseBody, ProgramDepositBody, ProgramOutputBody,
        ProgramRegisterRequestBody, ProgramRegisterResponseBody, ProgramStepProveRequestBody,
        ProgramStepProveResponseBody, ProgramStepRequestBody, ProgramStepResponseBody,
    },
    wallet::fund::{WalletFundRequestBody, WalletFundResponseBody},
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_node::config::RunConfig;
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, NodeHttpClient, TopologyConfig as TfTopologyConfig,
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::{
    common::manual_cluster::{
        LocalManualClusterHarnessBase, ManualNodeLayout, api_url, get_wallet_balance,
        start_local_manual_cluster_with_layout, wait_for_nodes_height, wait_for_spendable,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use serde::{Serialize, de::DeserializeOwned};
use serial_test::serial;
use testing_framework_core::scenario::{DynError, StartedNode};
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
    let (channel_id, _) = create_channel(&validator.client, &mut block_stream, funding_pk).await;

    // The deposit is funded from the inscription's change, so the wallet has
    // to apply the block including the inscription first, and that block has
    // to get deep enough for the change to be spent.
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
    wait_for_spendable(&validator.client, funding_pk, Duration::from_mins(2)).await;

    let deposit_metadata =
        Metadata::new_unchecked(format!("Mint {deposit_amount} to Alice in Zone").into_bytes());
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
    let (channel_id, parent) =
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
        .post(api_url(&validator.client, "channel/deposit"))
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
    let ChannelDepositResponseBody {
        hash: deposit_tx_hash,
        ..
    } = response.json().await.expect("deposit response");
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
    assert!(matches!(first_step, Step::User(_)));
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
        step.inputs().len(),
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
            vec![
                StepProof::ZkTransfer(first_proof),
                StepProof::ZkTransfer(proof),
            ],
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

    // A program deposit pays its value to the program out of the change, and
    // its proof is bound to the deposit and to that value.
    let deposited = 2;
    let ChannelStepResponseBody {
        step: deposit_step,
        proof: deposit_proof,
        outputs: deposit_outputs,
        ..
    } = validator
        .client
        .channel_step(ChannelStepRequestBody {
            program_deposit: Some(ProgramDepositBody {
                program_id: Fr::from(7u64),
                value: deposited,
                intent: Fr::from(11u64),
            }),
            payments: vec![],
            ..user_step_request(channel_id, funding_pk, vec![], 0)
        })
        .await
        .expect("a program deposit should be proven from the change");
    let Step::ProgramDeposit(program_deposit) = deposit_step else {
        panic!("the step should be a program deposit");
    };
    assert_eq!(program_deposit.value, deposited);
    assert_eq!(
        deposit_outputs
            .iter()
            .map(|note| (note.value, note.public_key))
            .collect::<Vec<_>>(),
        vec![(change - deposited, funding_pk)]
    );
    let verify = |excess, msg| {
        batch_verify(
            defer_zk_transfer(
                &deposit_proof,
                &program_deposit.inputs,
                &program_deposit.outputs,
                excess,
                program_deposit.commitment_merkle_root,
                msg,
            )
            .expect("the deposit should fit a ZkTransfer"),
        )
    };
    let msg = program_deposit_id(&channel_id, &program_deposit);
    verify(deposited, msg).expect("the proof should verify for the deposit and its value");
    verify(deposited + 1, msg).expect_err("the proof should not verify for another value");
    verify(deposited, Fr::from(1u64)).expect_err("the proof should not verify for another message");

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

/// 1. Register a program, create a channel and deposit into it.
/// 2. Inscribe a program deposit from the node's wallet together with the
///    program step that consumes it into a note of the program.
/// 3. Inscribe a second program step that consumes that note and pays it to a
///    user key, once the wallet follows the note and it is deep enough.
/// 4. Withdraw the paid note, which the wallet holds only if it followed the
///    outputs of the program step.
#[tokio::test]
#[serial]
async fn channel_program_step() {
    let deposit_amount = 5;
    let (_base, nodes, funding_pk) =
        start_channel_cluster("channel_program_step", deposit_amount).await;
    let node = &nodes[0].client;
    let mut block_stream = node.blocks_stream().await.unwrap();

    // The program accepts any public inputs, so its proofs only need the
    // values the ledger checks them against.
    let (verification_key, proving_key) = three_input_setup(1);
    let program_proof = |public: [Fr; 3]| {
        ProgramProof::new(
            CompressedGroth16Proof::try_from(&three_input_prove(&proving_key, public, 5))
                .expect("the program proof compresses"),
        )
    };
    let ProgramRegisterResponseBody {
        hash: register_tx_hash,
        program_id,
    } = post_until_success(
        node,
        "program/register",
        &ProgramRegisterRequestBody {
            tip: None,
            verification_key: ProgramVerificationKey::try_from(&verification_key)
                .expect("the setup key is a program key"),
            change_public_key: funding_pk,
            funding_public_keys: vec![funding_pk],
            max_tx_fee: GasCost::new(MAX_CHANNEL_TX_FEE),
        },
    )
    .await;
    wait_for_tx_inclusion(&mut block_stream, register_tx_hash, "program register").await;

    let (channel_id, parent) = create_channel(node, &mut block_stream, funding_pk).await;
    deposit(
        node,
        &mut block_stream,
        channel_id,
        funding_pk,
        deposit_amount,
    )
    .await;

    // A program deposit of the holder, then the program step consuming it.
    let paid = 3;
    let ChannelStepResponseBody {
        step: deposit_step,
        proof: deposit_proof,
        ..
    } = post_until_success(
        node,
        "channel/step",
        &ChannelStepRequestBody {
            tip: None,
            channel_id,
            appended: Vec::new(),
            payments: Vec::new(),
            program_deposit: Some(ProgramDepositBody {
                program_id,
                value: paid,
                intent: Fr::from(7u64),
            }),
            inscription: Inscription::new_unchecked(b"program deposit".to_vec()),
            change_public_key: funding_pk,
            holder_public_keys: vec![funding_pk],
        },
    )
    .await;
    let Step::ProgramDeposit(program_deposit) = deposit_step.clone() else {
        panic!("a step with a program deposit is a program deposit");
    };
    let ProgramStepResponseBody {
        tip,
        step,
        program_notes,
        paid_notes,
    } = post_until_success(
        node,
        "channel/program-step",
        &ProgramStepRequestBody {
            tip: None,
            channel_id,
            program_id,
            inputs: Vec::new(),
            program_outputs: vec![ProgramOutputBody {
                value: paid,
                stake_public_key: funding_pk,
                data: Fr::from(1u64),
            }],
            payments: Vec::new(),
            inscription: Inscription::new_unchecked(b"program step".to_vec()),
        },
    )
    .await;
    let program_note = step.program_outputs[0];
    let op = channel_inscription(
        channel_id,
        parent,
        vec![deposit_step, Step::Program(step.clone())],
    );
    let ProgramStepProveResponseBody { spend_proof } = post_until_success(
        node,
        "channel/program-step/prove",
        &ProgramStepProveRequestBody {
            tip,
            op: op.clone(),
            step_index: 1,
            program_notes: program_notes.clone(),
            paid_notes,
        },
    )
    .await;
    let step_proof = StepProof::ProgramStep(ProgramStepProof {
        spend_proof,
        program_proof: program_proof([
            step.program_step_id,
            program_deposits_hash(&[&program_deposit]),
            fr_from_mod_bytes(&op.op_id()),
        ]),
        program_notes,
    });
    let parent = op.id();
    inscribe(
        node,
        &mut block_stream,
        funding_pk,
        op,
        vec![StepProof::ZkTransfer(deposit_proof), step_proof],
    )
    .await;

    // The note of the program is consumed once the wallet adopted it and its
    // block is deep enough; the step pays it to a user key.
    let ProgramStepResponseBody {
        tip,
        step,
        program_notes,
        paid_notes,
    } = post_until_success(
        node,
        "channel/program-step",
        &ProgramStepRequestBody {
            tip: None,
            channel_id,
            program_id,
            inputs: vec![program_note],
            program_outputs: Vec::new(),
            payments: vec![ChannelPayment {
                public_key: funding_pk,
                value: paid,
            }],
            inscription: Inscription::new_unchecked(b"program payout".to_vec()),
        },
    )
    .await;
    let op = channel_inscription(channel_id, parent, vec![Step::Program(step.clone())]);
    let ProgramStepProveResponseBody { spend_proof } = post_until_success(
        node,
        "channel/program-step/prove",
        &ProgramStepProveRequestBody {
            tip,
            op: op.clone(),
            step_index: 0,
            program_notes: program_notes.clone(),
            paid_notes: paid_notes.clone(),
        },
    )
    .await;
    let step_proof = StepProof::ProgramStep(ProgramStepProof {
        spend_proof,
        program_proof: program_proof([
            step.program_step_id,
            program_deposits_hash(&[]),
            fr_from_mod_bytes(&op.op_id()),
        ]),
        program_notes,
    });
    inscribe(node, &mut block_stream, funding_pk, op, vec![step_proof]).await;

    // The wallet holds the note the program paid.
    let ChannelWithdrawResponseBody { outputs, .. } = post_until_success(
        node,
        "channel/withdraw",
        &ChannelWithdrawRequestBody {
            tip: None,
            channel_id,
            notes: vec![paid_notes[0].commitment()],
            change_public_key: funding_pk,
            funding_public_keys: vec![funding_pk],
            max_tx_fee: GasCost::new(MAX_CHANNEL_TX_FEE),
        },
    )
    .await;
    assert_eq!(
        outputs
            .iter()
            .map(|note| (note.value, note.public_key))
            .collect::<Vec<_>>(),
        vec![(paid, funding_pk)]
    );
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
        program_deposit: None,
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

// Two nodes producing blocks, the first holding a note of `deposit_amount` and
// a fee note under the key returned.
async fn start_channel_cluster(
    test_name: &str,
    deposit_amount: u64,
) -> (
    LocalManualClusterHarnessBase,
    Vec<StartedNode<LbcEnv>>,
    ZkPublicKey,
) {
    let (wallet_config, funding_pk, _funding_account) =
        channel_deposit_wallet_config(deposit_amount, FEE_NOTE_AMOUNT);
    let (base, nodes) = start_local_manual_cluster_with_layout(
        &test_name.replace('_', "-"),
        "mantle-channel",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(2)
                .with_allow_multiple_genesis_tokens(true)
                .with_test_context(Some(test_name.to_owned())),
        )
        .with_wallet_config(wallet_config),
        2,
        ManualNodeLayout::SelectNodeSeed(0),
        |config| Ok::<_, DynError>(channel_test_config(config)),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    )
    .await;
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
    (base, nodes, funding_pk)
}

// Deposits `amount` into the channel to `funding_pk` and waits for its
// inclusion.
async fn deposit(
    node: &NodeHttpClient,
    block_stream: &mut (impl futures::Stream<Item = ProcessedBlockEvent> + Unpin),
    channel_id: ChannelId,
    funding_pk: ZkPublicKey,
    amount: u64,
) {
    wait_for_spendable(node, funding_pk, Duration::from_mins(2)).await;
    let ChannelDepositResponseBody { hash, .. } = post_until_success(
        node,
        "channel/deposit",
        &ChannelDepositRequestBody {
            tip: None,
            channel_id,
            amount,
            holder_public_key: funding_pk,
            metadata: Metadata::new_unchecked(b"deposit".to_vec()),
            change_public_key: funding_pk,
            funding_public_keys: vec![funding_pk],
            max_tx_fee: GasCost::new(MAX_CHANNEL_TX_FEE),
        },
    )
    .await;
    wait_for_tx_inclusion(block_stream, hash, "deposit").await;
}

// Posts `body` to `path` until the node accepts it: a note the request spends
// is refused until the wallet adopted it and its block is deep enough.
async fn post_until_success<Request: Serialize + Sync, Response: DeserializeOwned>(
    node: &NodeHttpClient,
    path: &str,
    body: &Request,
) -> Response {
    timeout(Duration::from_mins(2), async {
        loop {
            let response = reqwest::Client::new()
                .post(api_url(node, path))
                .json(body)
                .send()
                .await
                .expect("request should not fail");
            if response.status().is_success() {
                return response
                    .json::<Response>()
                    .await
                    .expect("response should be valid JSON");
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {path} to succeed"))
}

fn channel_signing_key() -> Ed25519Key {
    Ed25519Key::from_bytes(&[7u8; 32])
}

// An inscription of the channel carrying `steps` in one mini-block.
fn channel_inscription(channel_id: ChannelId, parent: MsgId, steps: Vec<Step>) -> InscriptionOp {
    InscriptionOp {
        channel_id,
        inscription: Inscription::new_unchecked(b"zone block".to_vec()),
        parent,
        signer: channel_signing_key().public_key().into_unverified(),
        mini_blocks: MiniBlocks::from([
            MiniBlock::try_from(steps).expect("the steps fit a mini-block")
        ]),
    }
}

// Funds `op` from the node's wallet, signs it with the proofs of its steps,
// submits it and waits for its inclusion.
async fn inscribe(
    node: &NodeHttpClient,
    block_stream: &mut (impl futures::Stream<Item = ProcessedBlockEvent> + Unpin),
    funding_pk: ZkPublicKey,
    op: InscriptionOp,
    step_proofs: Vec<StepProof>,
) {
    let tx_builder = MantleTxBuilder::new()
        .push_op(Op::ChannelInscribe(op))
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
        OpProof::ChannelInscribe(ChannelInscribeProof::new(
            channel_signing_key().sign_payload(tx_hash.as_signing_bytes()),
            step_proofs,
        )),
        transfer_proof.expect("funding at non-zero gas prices should add a transfer"),
    ]);
    let signed_tx = SignedOps::from_parts(ops, op_proofs).expect("funded tx should be valid");
    node.submit_transaction(&signed_tx)
        .await
        .expect("inscription should be submitted");
    wait_for_tx_inclusion(block_stream, tx_hash, "inscription").await;
}

// Creates a channel with an inscription the node's wallet funds from
// `funding_pk`, and waits for its inclusion. Returns the channel with its tip.
async fn create_channel(
    node: &NodeHttpClient,
    block_stream: &mut (impl futures::Stream<Item = ProcessedBlockEvent> + Unpin),
    funding_pk: ZkPublicKey,
) -> (ChannelId, MsgId) {
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
    (channel_id, tip)
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
