use lb_core::{
    header::HeaderId,
    mantle::{
        Note, Op, SignedOps, Utxo,
        channel::{SlotTimeframe, SlotTimeout},
        ledger::{BoundedInputs, verification_mode::StandardMode},
        ops::{
            OpProof, OpProofRef, OpRef,
            channel::{
                MsgId, UnverifiedChannelKeys, VerifiedChannelKeys,
                config::ChannelConfigOp,
                deposit::DepositOp,
                inscribe::{Inscription, InscriptionOp},
                withdraw::ChannelWithdrawOp,
            },
        },
        transactions::{OpProofs, Ops, states::Unverified},
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
use num_bigint::BigUint;
use rand::{RngCore as _, thread_rng};
use tokio::sync::{mpsc, watch};

use super::{
    super::{
        state::PendingBundle,
        types::{FinalizedOp, InscriptionInfo, SequencerConfig},
        zone_sequencer::track_pending_tx,
    },
    *,
};
use crate::test_support::{
    MockNode, StreamEnd, StreamScript, api_block, funding_config, header_id, live_event, scripts,
    single_key_channel_state, unverified_tx_with_ops,
};

#[must_use]
pub fn utxo_with_sk() -> (ZkKey, Utxo) {
    let mut op_id = [0u8; 32];
    thread_rng().fill_bytes(&mut op_id);
    let zk_sk = ZkKey::from(BigUint::from(0u64));
    let utxo = Utxo {
        op_id,
        output_index: 0,
        note: Note::new(10, zk_sk.to_public_key()),
    };

    (zk_sk, utxo)
}

#[tokio::test]
async fn prepare_submit_deposit_and_inscription() {
    // Init a sequencer
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let (node, mut posted_txs) = MockNode::with_posted_channel();
    let mut sequencer =
        ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

    // Drive sequencer until ready
    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    // Prepare a deposit op
    let (sk, utxo) = utxo_with_sk();
    let deposit_op = DepositOp {
        channel_id,
        inputs: BoundedInputs::from(utxo.id()).into(),
        metadata: b"to Alice".into(),
    };

    // Build a `MantleTx` via the handle
    let (tx, msg_id, inscription_sig) = sequencer
        .handle()
        .prepare_tx(
            [Op::ChannelDeposit(deposit_op.clone())].into(),
            b"Mint 10 to Alice".into(),
        )
        .unwrap();
    assert_eq!(tx.inner().len(), 2);
    assert_eq!(tx.inner()[0], Op::ChannelDeposit(deposit_op));
    assert!(matches!(tx.inner()[1], Op::ChannelInscribe(_)));

    // Sign the `MantleTx`
    let op_proofs = OpProofs::from([
        OpProof::ZkSig(
            ZkKey::multi_sign(std::slice::from_ref(&sk), &tx.clone().hash().to_fr()).unwrap(),
        ),
        OpProof::Ed25519Sig(inscription_sig),
    ]);
    let signed_tx = SignedOps::from_parts(tx, op_proofs)
        .expect("Should generate a valid transaction with valid matching proofs.");

    // Submit via the handle (mutates state + queues post to in_flight).
    let (result, checkpoint) = sequencer
        .handle()
        .submit_signed_tx(signed_tx.clone(), msg_id)
        .unwrap();
    assert_eq!(result.inscription_id(), signed_tx.hash());
    assert_eq!(checkpoint.last_msg_id, msg_id);

    // The post lives in `in_flight` until the drive loop polls it.
    // Drive `next_event` concurrently with the recv so the post future
    // runs and MockNode delivers to `posted_txs`.
    tokio::select! {
        tx = posted_txs.recv() => assert_eq!(tx.unwrap(), signed_tx),
        () = async {
            loop {
                drop(sequencer.next_event().await);
            }
        } => unreachable!(),
    }
}

/// A prepared tx pins the parent at prepare time; a publish in between
/// takes that position, so the stale tx is refused instead of competing.
#[tokio::test]
async fn submit_signed_tx_refuses_a_parent_with_a_pending_child() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let (node, _posted_txs) = MockNode::with_posted_channel();
    let mut sequencer =
        ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);
    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    let (tx, msg_id, inscription_sig) = sequencer
        .handle()
        .prepare_tx(Ops::new_unchecked(Vec::new()), b"prepared first".into())
        .unwrap();
    let signed_tx =
        SignedOps::from_parts(tx, OpProofs::from([OpProof::Ed25519Sig(inscription_sig)]))
            .expect("a valid inscription-only tx");

    sequencer
        .handle()
        .publish(b"published in between".into())
        .await
        .unwrap();

    let result = sequencer.handle().submit_signed_tx(signed_tx, msg_id);
    assert!(
        matches!(result, Err(Error::ChannelStateChanged(_))),
        "stale parent must be refused, got {result:?}"
    );
    assert_eq!(sequencer.state.as_ref().unwrap().pending_publish_count(), 1);
}

/// A gap block's deposit surfaces once, in order, even when the first
/// backfill failed, the stream reconnected and the chain switched to
/// another fork and back before the re-delivered event backfills the gap.
#[tokio::test]
#[expect(clippy::too_many_lines, reason = "Test function.")]
async fn gap_deposits_surface_after_a_failed_backfill_and_a_fork_switch() {
    use std::{
        collections::HashMap,
        sync::{Arc, atomic::AtomicUsize},
    };

    use lb_core::{
        events::DepositNote,
        mantle::{
            ledger::NoteId,
            ops::{OpId as _, channel::deposit::Metadata},
        },
    };
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::ZkPublicKey;

    use crate::test_support::{deposit_event, inscribe_op};

    // G(0) <- B1(A) <- B2(Y, D2) <- B3(D3)   canonical in the end
    //             \- C(Z)                    canonical in between
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let pk = ZkPublicKey::from(Fr::from(7u64));
    let deposit = |n: u32| {
        let op = DepositOp {
            channel_id,
            inputs: BoundedInputs::from(NoteId::from(Fr::from(n))).into(),
            metadata: Metadata::try_from(vec![u8::try_from(n).unwrap()]).unwrap(),
        };
        let tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(op.clone())]);
        let note = DepositNote {
            note_id: NoteId::from(Fr::from(1000 + u64::from(n))),
            value: 50,
            pk,
        };
        let event = deposit_event(&tx, &op, 50, vec![note]);
        (op.op_id(), tx, event)
    };
    let a = inscribe_op(channel_id, MsgId::root(), b"a");
    let y = inscribe_op(channel_id, a.id(), b"y");
    let z = inscribe_op(channel_id, a.id(), b"z");
    let (y_id, z_id) = (y.id(), z.id());
    let (d2, d2_tx, d2_event) = deposit(2);
    let (d3, d3_tx, d3_event) = deposit(3);
    let b1 = api_block(
        1,
        0,
        1,
        vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(a)])],
    );
    let b2 = api_block(
        2,
        1,
        2,
        vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(y)]), d2_tx],
    );
    let c = api_block(
        9,
        1,
        2,
        vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(z)])],
    );
    let b3 = api_block(3, 2, 3, vec![d3_tx]);
    let node = MockNode {
        scripts: scripts(vec![
            StreamScript {
                events: vec![live_event(&b1), live_event(&b3)],
                then: StreamEnd::Hang,
            },
            StreamScript {
                events: vec![live_event(&c), live_event(&b3)],
                then: StreamEnd::Hang,
            },
        ]),
        blocks: vec![b2],
        block_fetch_failures: Arc::new(AtomicUsize::new(1)),
        events: HashMap::from([(header_id(2), d2_event), (header_id(3), d3_event)]),
        ..MockNode::default()
    };
    let config = SequencerConfig {
        reconnect_delay: std::time::Duration::from_millis(20),
        ..SequencerConfig::new(funding_config())
    };
    let mut sequencer =
        ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
    let msg_ids = |txs: &[ChannelUpdateTx]| {
        txs.iter()
            .filter_map(|tx| tx.inscription().map(|info| info.this_msg))
            .collect::<Vec<_>>()
    };

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut before = Vec::new();
    let (update, deposits) = loop {
        let event = tokio::time::timeout_at(deadline, sequencer.next_event())
            .await
            .expect("B3 lands after the reconnect and the fork switch");
        let Event::BlocksProcessed {
            channel_update,
            deposits,
            ..
        } = event
        else {
            continue;
        };
        if deposits.iter().any(|d| d.op_id == d2) {
            break (channel_update, deposits);
        }
        before.push(channel_update);
    };

    assert_eq!(sequencer.current_tip, Some(header_id(3)));
    assert!(
        before.iter().all(|u| !msg_ids(u.adopted()).contains(&y_id)),
        "Y is only adopted with the gap"
    );
    let switched_to_fork = before.last().expect("C was processed before B3");
    assert_eq!(msg_ids(switched_to_fork.adopted()), vec![z_id]);
    let observed: Vec<_> = deposits.iter().map(|d| d.op_id).collect();
    assert_eq!(
        observed,
        vec![d2, d3],
        "gap deposit first, then the live block's"
    );
    assert_eq!(msg_ids(update.adopted()), vec![y_id]);
    assert_eq!(msg_ids(update.orphaned()), vec![z_id]);
}

/// Drive a sequencer with `stale_refund_slots = window` through a
/// publish and two LIB advances; returns how many fund calls the node
/// saw within `for_at_most` and the checkpoint published last. Posts
/// are no signal: the resubmit pass re-posts every unmined entry on each
/// tick regardless.
async fn drive_stale_publish(
    window: u64,
    for_at_most: std::time::Duration,
) -> (usize, SequencerCheckpoint) {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let (up_tx, up_rx) = watch::channel(true);
    let (fees_tx, mut fees_rx) = mpsc::channel(16);
    let lib_event =
        |block: &lb_common_http_client::ApiBlock, lib: u8, lib_slot: u64| ProcessedBlockEvent {
            block: block.clone(),
            tip: block.header.id,
            tip_slot: block.header.slot,
            lib: header_id(lib),
            lib_slot: Slot::from(lib_slot),
        };
    let b1 = api_block(1, 0, 1, Vec::new());
    let b2 = api_block(2, 1, 2, Vec::new());
    let b3 = api_block(3, 2, 9, Vec::new());
    // The second connection is gated behind `up` so the publish is in
    // before the LIB advances age it.
    let node = MockNode {
        up: Some(up_rx),
        funding_priority_fees: Some(fees_tx),
        scripts: scripts(vec![
            StreamScript {
                events: vec![live_event(&b1)],
                then: StreamEnd::Hang,
            },
            StreamScript {
                events: vec![lib_event(&b2, 1, 1), lib_event(&b3, 2, 8)],
                then: StreamEnd::Hang,
            },
        ]),
        ..MockNode::default()
    };
    let config = SequencerConfig {
        reconnect_delay: std::time::Duration::from_millis(20),
        resubmit_interval: std::time::Duration::from_millis(20),
        stale_refund_slots: window,
        ..SequencerConfig::new(funding_config())
    };
    let mut sequencer =
        ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
    let client = sequencer.client();

    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }
    let publish = client.publish(b"stale".into());
    tokio::select! {
        result = publish => drop(result.expect("publish is accepted after Ready")),
        () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
    }
    up_tx.send(false).unwrap();

    let deadline = tokio::time::Instant::now() + for_at_most;
    let reconnect = tokio::time::sleep(std::time::Duration::from_millis(100));
    tokio::pin!(reconnect);
    let mut fund_calls = 0;
    loop {
        tokio::select! {
            () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
            () = &mut reconnect => up_tx.send(true).unwrap(),
            Some(_) = fees_rx.recv() => fund_calls += 1,
            () = tokio::time::sleep_until(deadline) => break,
        }
        if fund_calls >= 2 {
            break;
        }
    }
    assert_eq!(
        sequencer.state.as_ref().unwrap().pending_publish_count(),
        1,
        "a rebuild replaces the entry, it never duplicates it"
    );
    let checkpoint = sequencer
        .subscribe_checkpoint()
        .borrow()
        .clone()
        .expect("a checkpoint was published");
    (fund_calls, checkpoint)
}

/// An own publish unmined past the window is re-funded once the LIB has
/// moved past it, and the checkpoint published right after carries the
/// rebuilt entry: its funding stamp is the LIB slot of the re-fund, not
/// of the publish.
#[tokio::test]
async fn stale_publish_is_refunded() {
    let (fund_calls, checkpoint) = drive_stale_publish(1, std::time::Duration::from_secs(5)).await;
    assert_eq!(fund_calls, 2, "the publish's funding, then the rebuild's");
    assert_eq!(checkpoint.funding.len(), 1);
    assert_eq!(
        checkpoint.funding[0].funded_at,
        Slot::from(8),
        "the checkpoint was republished after the re-fund"
    );
}

/// The pre-funding ops of a publish survive a checkpoint round trip
/// through serde and a restore, so a restored sequencer can still re-fund
/// what it restored.
#[tokio::test]
async fn funding_record_survives_a_checkpoint_round_trip() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let config = SequencerConfig::new(funding_config());
    let mut sequencer = ZoneSequencer::init_with_config(
        channel_id,
        sequencer_key.clone(),
        MockNode::default(),
        config.clone(),
        None,
    );
    let client = sequencer.client();
    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }
    let publish = client.publish(b"restored".into());
    let (result, checkpoint) = tokio::select! {
        result = publish => result.expect("publish is accepted after Ready"),
        () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
    };
    let tx_hash = result.inscription_id();
    assert_eq!(checkpoint.funding.len(), 1);
    assert_eq!(checkpoint.funding[0].tx_hash, tx_hash);
    assert!(checkpoint.funding[0].pre_fund.is_some());

    let json = serde_json::to_string(&checkpoint).expect("checkpoint serializes");
    let restored_checkpoint: SequencerCheckpoint =
        serde_json::from_str(&json).expect("checkpoint deserializes");
    let restored = ZoneSequencer::init_with_config(
        channel_id,
        sequencer_key,
        MockNode::default(),
        config,
        Some(restored_checkpoint),
    );

    let state = restored.state.as_ref().expect("restored state");
    let entry = state.pending_inscription(&tx_hash).expect("restored");
    assert!(
        entry.pre_fund.is_some(),
        "the restored entry keeps its pre-funding ops"
    );
    assert_eq!(
        entry.funded_at,
        Some(checkpoint.funding[0].funded_at),
        "and its stamp"
    );
}

#[tokio::test]
async fn disabled_refund_window_never_refunds() {
    let (fund_calls, checkpoint) = drive_stale_publish(0, std::time::Duration::from_secs(1)).await;
    assert_eq!(fund_calls, 1);
    assert_eq!(checkpoint.funding[0].funded_at, Slot::from(0));
}

#[tokio::test]
async fn cancelled_next_event_resumes_the_pulled_block() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let first_block = api_block(1, 0, 1, Vec::new());
    let second_block = api_block(2, 1, 2, Vec::new());
    let (gate_tx, gate_rx) = watch::channel(true);
    let (calls_tx, mut calls_rx) = mpsc::unbounded_channel();
    let node = MockNode {
        scripts: scripts(vec![StreamScript {
            events: vec![live_event(&first_block), live_event(&second_block)],
            then: StreamEnd::Hang,
        }]),
        channel_state_gate: Some(gate_rx),
        channel_state_calls: Some(calls_tx),
        ..MockNode::default()
    };
    let mut sequencer =
        ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    assert!(matches!(
        sequencer.next_event().await,
        Event::BlocksProcessed { .. }
    ));
    assert!(matches!(
        sequencer.next_event().await,
        Event::TurnNotification { .. }
    ));

    while calls_rx.try_recv().is_ok() {}
    gate_tx.send(false).unwrap();

    {
        let next_event = sequencer.next_event();
        tokio::pin!(next_event);

        tokio::select! {
            call = calls_rx.recv() => {
                call.expect("channel-state call should be observed");
            }
            event = &mut next_event => {
                panic!("block processing completed while its node request was gated: {event:?}");
            }
        }
    }

    assert_eq!(
        sequencer
            .pending_block_event
            .as_ref()
            .map(|event| event.block.header.id),
        Some(header_id(2))
    );

    gate_tx.send(true).unwrap();
    let resumed = tokio::time::timeout(std::time::Duration::from_secs(1), sequencer.next_event())
        .await
        .expect("the retained block should resume after cancellation");

    assert!(matches!(resumed, Event::BlocksProcessed { .. }));
    assert_eq!(sequencer.current_tip, Some(header_id(2)));
    assert!(sequencer.pending_block_event.is_none());

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), sequencer.next_event(),)
            .await
            .is_err(),
        "the resumed block must not be emitted twice"
    );
}

#[tokio::test]
async fn cancelled_finalized_backfill_restarts_without_partial_state() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let first_block = api_block(1, 0, 1, Vec::new());
    let second_block = api_block(2, 1, 2, Vec::new());
    let second_event = ProcessedBlockEvent {
        block: second_block.clone(),
        tip: second_block.header.id,
        tip_slot: second_block.header.slot,
        lib: first_block.header.id,
        lib_slot: first_block.header.slot,
    };
    let (gate_tx, gate_rx) = watch::channel(true);
    let (calls_tx, mut calls_rx) = mpsc::unbounded_channel();
    let node = MockNode {
        scripts: scripts(vec![StreamScript {
            events: vec![live_event(&first_block), second_event],
            then: StreamEnd::Hang,
        }]),
        immutable: vec![first_block.clone()],
        immutable_blocks_gate: Some(gate_rx),
        immutable_blocks_calls: Some(calls_tx),
        ..MockNode::default()
    };
    let mut sequencer =
        ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    assert!(matches!(
        sequencer.next_event().await,
        Event::BlocksProcessed { .. }
    ));
    assert!(matches!(
        sequencer.next_event().await,
        Event::TurnNotification { .. }
    ));

    while calls_rx.try_recv().is_ok() {}
    gate_tx.send(false).unwrap();

    {
        let next_event = sequencer.next_event();
        tokio::pin!(next_event);

        tokio::select! {
            call = calls_rx.recv() => {
                call.expect("immutable-blocks call should be observed");
            }
            event = &mut next_event => {
                panic!("finalized backfill completed while its node request was gated: {event:?}");
            }
        }
    }

    assert_eq!(sequencer.lib_slot, Slot::genesis());
    assert_eq!(sequencer.current_tip, Some(first_block.header.id));
    assert_eq!(
        sequencer
            .pending_block_event
            .as_ref()
            .map(|event| event.block.header.id),
        Some(second_block.header.id)
    );

    gate_tx.send(true).unwrap();
    let resumed = tokio::time::timeout(std::time::Duration::from_secs(1), sequencer.next_event())
        .await
        .expect("the finalized backfill should resume after cancellation");

    assert!(matches!(resumed, Event::BlocksProcessed { .. }));
    assert_eq!(sequencer.lib_slot, first_block.header.slot);
    assert_eq!(sequencer.current_tip, Some(second_block.header.id));
    assert!(sequencer.pending_block_event.is_none());
}

/// A `SequencerClient::publish` issued while the node is down (reconnect
/// in progress) must resolve promptly with [`Error::Unavailable`] —
/// funding needs the node — instead of blocking until connectivity is
/// restored. Once the node is back and `Ready` is re-announced,
/// publishing works again.
#[tokio::test]
async fn client_publish_fails_fast_during_reconnect_and_recovers() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let (up_tx, up_rx) = watch::channel(true);
    let (mut node, mut posted_txs) = MockNode::with_posted_channel();
    node.up = Some(up_rx);
    let config = SequencerConfig {
        reconnect_delay: std::time::Duration::from_millis(20),
        resubmit_interval: std::time::Duration::from_millis(20),
        ..SequencerConfig::new(funding_config())
    };
    let mut sequencer =
        ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
    let client = sequencer.client();

    // Drive until the sequencer has emitted `Ready`.
    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    // After `Ready` on this single-key channel the turn-to-write watch is
    // true; the stream drop clears it, giving a deterministic signal that
    // the sequencer observed the disconnect before we publish.
    let mut turn_rx = client.subscribe_turn_to_write();
    assert!(
        turn_rx.borrow_and_update().our_turn_to_write,
        "single-key channel must report our turn after Ready"
    );

    // Take the node down: the live stream ends and the sequencer enters
    // reconnect (subsequent `block_stream` calls error).
    up_tx.send(false).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if !turn_rx.borrow_and_update().our_turn_to_write {
                break;
            }
            tokio::select! {
                changed = turn_rx.changed() => changed.unwrap(),
                () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
            }
        }
    })
    .await
    .expect("stream drop must clear turn-to-write");

    // A client publish while the node is down must resolve promptly. We
    // drive `next_event` concurrently; the publish is serviced from inside
    // `wait_reconnect_delay` while the node is still down. With the old
    // behavior the request would never be drained during reconnect and this
    // would hang (caught by the timeout).
    let publish = client.publish(b"during-reconnect".into());
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            result = publish => result,
            () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
        }
    })
    .await
    .expect("client publish must resolve during reconnect, not block on connectivity");
    assert!(
        matches!(result, Err(Error::Unavailable { .. })),
        "publish while disconnected must fail fast, got {result:?}"
    );

    // Bring the node back up and wait for the re-announced `Ready`.
    up_tx.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if matches!(sequencer.next_event().await, Event::Ready) {
                break;
            }
        }
    })
    .await
    .expect("Ready should be re-announced after reconnect");

    // Publishing works again and the inscription is posted.
    let publish = client.publish(b"after-reconnect".into());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            result = publish => result,
            () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
        }
    })
    .await
    .expect("client publish must resolve after reconnect")
    .expect("publish should succeed after reconnect");
    let posted = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            tx = posted_txs.recv() => tx,
            () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
        }
    })
    .await
    .expect("inscription should be posted after reconnect")
    .expect("posted_txs channel should be open");

    assert!(
        posted
            .op_refs()
            .into_iter()
            .any(|op| matches!(op, OpRef::ChannelInscribe(_))),
        "posted tx should carry the inscription published during reconnect"
    );
}

/// `TurnNotification` reaches `next_event` callers and the events
/// broadcast once each, in the same order, after the block that changed
/// the turn; the turn watch flips as soon as the change is detected.
#[tokio::test]
async fn next_event_yields_turn_notifications() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let (up_tx, up_rx) = watch::channel(true);
    let (mut node, _posted_txs) = MockNode::with_posted_channel();
    node.up = Some(up_rx);
    let config = SequencerConfig {
        reconnect_delay: std::time::Duration::from_millis(20),
        resubmit_interval: std::time::Duration::from_millis(20),
        ..SequencerConfig::new(funding_config())
    };
    let mut sequencer =
        ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
    let mut events_rx = sequencer.subscribe_events();
    let mut turn_rx = sequencer.subscribe_turn_to_write();
    turn_rx.mark_unchanged();

    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    // The watch already reflects the turn before either channel delivers
    // the event.
    assert!(turn_rx.has_changed().unwrap());
    assert!(turn_rx.borrow_and_update().our_turn_to_write);

    // Single-key channel: the block that made us ready also made it our
    // turn. `BlocksProcessed` comes first, then the turn.
    let first = sequencer.next_event().await;
    assert!(
        matches!(first, Event::BlocksProcessed { .. }),
        "block event should precede the turn change, got {first:?}"
    );
    let second = sequencer.next_event().await;
    let Event::TurnNotification { notification } = second else {
        panic!("expected TurnNotification after the block event, got {second:?}");
    };
    assert!(notification.our_turn_to_write);

    // The broadcast carries the same events, once each, in the same order.
    let mut broadcast = Vec::new();
    while let Ok(event) = events_rx.try_recv() {
        broadcast.push(event);
    }
    let kinds: Vec<_> = broadcast
        .iter()
        .map(|event| match event {
            Event::Ready => "ready",
            Event::BlocksProcessed { .. } => "block",
            Event::TurnNotification { notification } if notification.our_turn_to_write => {
                "our turn"
            }
            Event::TurnNotification { .. } => "not our turn",
        })
        .collect();
    assert_eq!(
        kinds,
        ["not our turn", "block", "ready", "block", "our turn"],
        "broadcast: {broadcast:?}"
    );

    // A stream drop clears the turn; that change is returned too, and
    // broadcast exactly once.
    up_tx.send(false).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Event::TurnNotification { notification } = sequencer.next_event().await
                && !notification.our_turn_to_write
            {
                break;
            }
        }
    })
    .await
    .expect("stream drop must yield a not-our-turn notification via next_event");
    let mut cleared = 0;
    while let Ok(event) = events_rx.try_recv() {
        if let Event::TurnNotification { notification } = event {
            assert!(!notification.our_turn_to_write);
            cleared += 1;
        }
    }
    assert_eq!(
        cleared, 1,
        "the cleared turn must be broadcast exactly once"
    );
}

/// The turn is re-evaluated on its own slot boundary: with no block after
/// the first one, `next_event` still yields the alternating turn changes.
#[tokio::test]
async fn turn_notification_fires_on_timeframe_boundary_without_blocks() {
    assert_turns_alternate_without_blocks(1, 0).await;
}

/// Same for the timeout rotation, anchored at the last landed inscription.
#[tokio::test]
async fn turn_notification_fires_on_timeout_boundary_without_blocks() {
    assert_turns_alternate_without_blocks(0, 1).await;
}

async fn assert_turns_alternate_without_blocks(posting_timeframe: u32, posting_timeout: u32) {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let mut channel = single_key_channel_state();
    channel.accredited_keys = UnverifiedChannelKeys::try_from(vec![
        sequencer_key.public_key().into_unverified(),
        Ed25519Key::from_bytes(&[1; 32])
            .public_key()
            .into_unverified(),
    ])
    .unwrap()
    .into();
    channel.posting_timeframe = posting_timeframe.into();
    channel.posting_timeout = posting_timeout.into();
    let node = MockNode {
        channel_state: Some(channel),
        slot_duration_ms: 100,
        ..MockNode::default()
    };
    let config = SequencerConfig {
        resubmit_interval: std::time::Duration::from_secs(600),
        ..SequencerConfig::new(funding_config())
    };
    let mut sequencer =
        ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    let started = std::time::Instant::now();
    let mut turns = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while turns.len() < 4 {
            if let Event::TurnNotification { notification } = sequencer.next_event().await {
                turns.push(notification.our_turn_to_write);
            }
        }
    })
    .await
    .expect("turn changes must arrive without blocks");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "turn changes should follow the 100ms slots, took {:?}",
        started.elapsed()
    );
    assert!(
        turns.windows(2).all(|pair| pair[0] != pair[1]),
        "a two-key channel rotating every slot alternates: {turns:?}"
    );
}

/// With a publish margin the turn watch closes before the rotation, at
/// the same slot `can_publish_inscription_now` starts refusing.
#[tokio::test]
async fn turn_notification_closes_with_the_publish_margin() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let mut channel = single_key_channel_state();
    channel.posting_timeframe = 4u32.into();
    let node = MockNode {
        channel_state: Some(channel),
        slot_duration_ms: 300,
        ..MockNode::default()
    };
    let config = SequencerConfig {
        min_slots_remaining_in_turn: 3,
        resubmit_interval: std::time::Duration::from_secs(600),
        ..SequencerConfig::new(funding_config())
    };
    let mut sequencer =
        ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    // Single key: every turn is ours, open for the first two of its four
    // slots and closed for the last two. The notification at Ready is an
    // observation mid-turn; the first close is the first boundary.
    let mut seen = 0;
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while seen < 4 {
            let Event::TurnNotification { notification } = sequencer.next_event().await else {
                continue;
            };
            if seen == 0 && notification.our_turn_to_write {
                continue;
            }
            assert_eq!(
                notification.our_turn_to_write,
                sequencer.can_publish_inscription_now(),
                "watch and publish gate disagree: {notification:?}"
            );
            let current = notification.current_slot.unwrap();
            let expected = if notification.our_turn_to_write {
                notification.starting_slot.unwrap()
            } else {
                notification.ends_at_slot.unwrap() - 3 + 1
            };
            assert_eq!(
                current, expected,
                "flipped at the wrong slot: {notification:?}"
            );
            seen += 1;
        }
    })
    .await
    .expect("turn changes must arrive without blocks");
}

/// A `submit_signed_tx` bundle chains subsequent publishes off its last
/// inscription — the config in it moves only the config lineage.
/// Otherwise the next publish claims the same channel position as the
/// bundle and the two race, permanently invalidating one side.
#[tokio::test]
async fn publish_after_bundle_chains_on_the_bundle_inscription_tip() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let node = MockNode::default();
    let mut sequencer = ZoneSequencer::init(
        channel_id,
        sequencer_key.clone(),
        node,
        funding_config(),
        None,
    );

    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    let inscribe = InscriptionOp {
        channel_id,
        inscription: b"bundle".to_vec().try_into().unwrap(),
        parent: MsgId::root(),
        signer: sequencer_key.public_key().into_unverified(),
    };
    let config = ChannelConfigOp {
        channel: channel_id,
        parent: MsgId::root(),
        keys: VerifiedChannelKeys::try_from(vec![sequencer_key.public_key()]).unwrap(),
        posting_timeframe: SlotTimeframe::from(0u32),
        posting_timeout: SlotTimeout::from(0u32),
        configuration_threshold: 1,
        transfer_threshold: 1,
    };
    let inscribe_msg = inscribe.id();
    let bundle = unverified_tx_with_ops(vec![
        Op::ChannelInscribe(inscribe),
        Op::ChannelConfig(config),
    ]);

    let (result, _cp) = sequencer
        .handle()
        .submit_signed_tx(bundle, inscribe_msg)
        .expect("bundle submit should be accepted");
    assert_eq!(
        result.tx.inscription().this_msg,
        inscribe_msg,
        "the bundle's resulting tip is its inscription"
    );

    let (published, _cp) = sequencer
        .handle()
        .publish(b"after-bundle".into())
        .await
        .expect("publish after bundle should be accepted");
    assert_eq!(
        published.tx.inscription().parent_msg,
        inscribe_msg,
        "the next publish must chain after the pending bundle's inscription"
    );
}

#[tokio::test]
async fn config_only_block_orphans_pending_inscription_but_keeps_message_tip() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);

    let config_op = ChannelConfigOp {
        channel: channel_id,
        parent: MsgId::root(),
        keys: VerifiedChannelKeys::try_from(vec![Ed25519Key::from_bytes(&[0; 32]).public_key()])
            .unwrap(),
        posting_timeframe: SlotTimeframe::from(0u32),
        posting_timeout: SlotTimeout::from(0u32),
        configuration_threshold: 1,
        transfer_threshold: 1,
    };
    let config_tx = unverified_tx_with_ops(vec![Op::ChannelConfig(config_op)]);
    let config_hash = config_tx.hash();
    let config_block = api_block(2, 1, 2, vec![config_tx]);

    // Second connection (the config block) is gated behind `up` so it
    // cannot be consumed before the publish is in.
    let (up_tx, up_rx) = watch::channel(true);
    let node = MockNode {
        up: Some(up_rx),
        scripts: scripts(vec![
            StreamScript {
                events: vec![live_event(&api_block(1, 0, 1, Vec::new()))],
                then: StreamEnd::Hang,
            },
            StreamScript {
                events: vec![live_event(&config_block)],
                then: StreamEnd::Hang,
            },
        ]),
        ..MockNode::default()
    };
    let config = SequencerConfig {
        reconnect_delay: std::time::Duration::from_millis(20),
        resubmit_interval: std::time::Duration::from_millis(20),
        ..SequencerConfig::new(funding_config())
    };
    let mut sequencer =
        ZoneSequencer::init_with_config(channel_id, sequencer_key, node, config, None);
    let client = sequencer.client();

    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }

    let publish = client.publish(b"survives-config".into());
    let (result, _checkpoint) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            result = publish => result,
            () = async { loop { drop(sequencer.next_event().await); } } => unreachable!(),
        }
    })
    .await
    .expect("publish must resolve")
    .expect("publish should be accepted after Ready");
    let p_hash = result.inscription_id();

    // Keep driving between the toggles so the down-edge is observed. The
    // config block is recognized by its config entering `adopted`; that
    // `BlocksProcessed` carries the state to assert on.
    up_tx.send(false).unwrap();
    let (checkpoint, update) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let toggle = async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            up_tx.send(true).unwrap();
        };
        let drive = async {
            loop {
                if let Event::BlocksProcessed {
                    checkpoint,
                    channel_update,
                    ..
                } = sequencer.next_event().await
                    && channel_update
                        .adopted()
                        .iter()
                        .any(|tx| tx.tx_hash() == config_hash)
                {
                    return (checkpoint, channel_update);
                }
            }
        };
        let ((), out) = tokio::join!(toggle, drive);
        out
    })
    .await
    .expect("the config-only block must be processed");

    // The config changes the channel view, so the pending inscription is
    // shed and reported orphaned (to be resubmitted against the new view).
    assert!(update.orphaned().iter().any(|tx| tx.tx_hash() == p_hash));
    assert!(checkpoint.pending_txs.iter().all(|(h, _)| *h != p_hash));
    assert!(
        update
            .canonical_chain()
            .is_some_and(|mut c| c.all(|tx| tx.tx_hash() != p_hash))
    );
    // The chaining pointer resets to the (unchanged) message tip so the
    // resubmit re-posts there. Nothing was mined, so the tip is root.
    assert_eq!(
        checkpoint.last_msg_id,
        MsgId::root(),
        "the pointer resets to the message tip (root)"
    );
}

#[test]
fn track_pending_tx_classifies_atomic_bundle_with_withdraws() {
    // Bundle: [ChannelWithdraw(channel_id), ChannelInscribe(channel_id)]
    // Restore should put it in pending (not pending_other) with the
    // withdraws field populated, so on orphan we emit
    // ChannelUpdateTx::AtomicWithdraw (not Inscription).
    use lb_core::mantle::NoteId;
    use lb_groth16::Fr;

    let channel_id = ChannelId::from([1u8; 32]);
    let withdraw_op = ChannelWithdrawOp {
        channel_id,
        inputs: BoundedInputs::from(NoteId::from(Fr::from(0u64))).into(),
    };
    let inscribe_op = InscriptionOp {
        channel_id,
        inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
        parent: MsgId::root(),
        signer: Ed25519Key::from_bytes(&[0; 32])
            .public_key()
            .into_unverified(),
    };
    let mantle_tx = Ops::from([
        Op::ChannelWithdraw(withdraw_op.clone()),
        Op::ChannelInscribe(inscribe_op),
    ]);
    let tx_hash = mantle_tx.hash();
    let signed_tx = SignedOps::from_ops_with_sample_proofs(mantle_tx);

    let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
    track_pending_tx(&mut state, signed_tx, channel_id).unwrap();

    let pending = state
        .pending_inscription(&tx_hash)
        .expect("bundle should be in pending inscriptions");
    let PendingBundle::Withdraw { withdraws, .. } = &pending.bundle else {
        panic!("bundle should be a withdraw bundle");
    };
    assert_eq!(withdraws.len(), 1, "bundle should carry one WithdrawInfo");
    assert_eq!(withdraws[0].op, withdraw_op);
    assert!(
        !state.pending_other_contains(&tx_hash),
        "bundle should not be in pending_other"
    );
}

#[test]
fn track_pending_tx_classifies_plain_inscription_with_none_withdraws() {
    // Plain inscription: pending with `withdraws == None`.
    let channel_id = ChannelId::from([2u8; 32]);
    let inscribe_op = InscriptionOp {
        channel_id,
        inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
        parent: MsgId::root(),
        signer: Ed25519Key::from_bytes(&[0; 32])
            .public_key()
            .into_unverified(),
    };
    let mantle_tx = Ops::from([Op::ChannelInscribe(inscribe_op)]);
    let tx_hash = mantle_tx.hash();
    let signed_tx = SignedOps::from_ops_with_sample_proofs(mantle_tx);

    let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
    track_pending_tx(&mut state, signed_tx, channel_id).unwrap();

    let pending = state
        .pending_inscription(&tx_hash)
        .expect("plain inscription should be in pending inscriptions");
    assert!(matches!(pending.bundle, PendingBundle::Plain));
}

#[test]
fn track_pending_tx_falls_back_to_other_when_no_inscribe_for_channel() {
    // Inscribe for a different channel: should fall back to pending_other
    // (treated as opaque).
    let our_channel = ChannelId::from([3u8; 32]);
    let other_channel = ChannelId::from([4u8; 32]);
    let inscribe_op = InscriptionOp {
        channel_id: other_channel,
        inscription: Inscription::try_from(b"hello".to_vec()).unwrap(),
        parent: MsgId::root(),
        signer: Ed25519Key::from_bytes(&[0; 32])
            .public_key()
            .into_unverified(),
    };
    let mantle_tx = Ops::from([Op::ChannelInscribe(inscribe_op)]);
    let tx_hash = mantle_tx.hash();
    let signed_tx = SignedOps::from_ops_with_sample_proofs(mantle_tx);

    let mut state = TxState::new(HeaderId::from([0; 32]), MsgId::root());
    track_pending_tx(&mut state, signed_tx, our_channel).unwrap();

    assert!(
        state.pending_inscription(&tx_hash).is_none(),
        "wrong-channel tx should not be in pending inscriptions"
    );
    assert!(
        state.pending_other_contains(&tx_hash),
        "wrong-channel tx should be in pending_other"
    );
}

/// Cold start with a channel inscription at slot 0 (genesis): the
/// sequencer must include that slot in its initial backfill and emit it
/// in a `Finalized` state change. Regression guard for the off-by-one fix
/// where `backfill_from = lib_slot + 1` silently skipped genesis.
#[tokio::test]
async fn cold_start_backfills_genesis_slot() {
    let channel_id = ChannelId::from([7; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);

    // A signed tx with a single ChannelInscribe on our channel at
    // genesis (parent_msg = root).
    let inscribe = InscriptionOp {
        channel_id,
        parent: MsgId::root(),
        inscription: Inscription::new_unchecked(Vec::new()),
        signer: sequencer_key.public_key().into_unverified(),
    };
    let expected_msg_id = inscribe.id();
    let genesis_tx = unverified_tx_with_ops(vec![Op::ChannelInscribe(inscribe)]);
    let genesis_tx_hash = genesis_tx.hash();

    let genesis_block = api_block(1, 0, 0, vec![genesis_tx]);
    // Empty block at slot 1 so the block stream advances and the
    // sequencer signals `Ready`, giving the test a clean exit signal.
    let live_block = api_block(2, 1, 1, Vec::new());

    let node = MockNode {
        lib: genesis_block.header.id,
        tip: genesis_block.header.id,
        scripts: scripts(vec![StreamScript {
            events: vec![ProcessedBlockEvent {
                block: live_block.clone(),
                tip: live_block.header.id,
                tip_slot: live_block.header.slot,
                lib: genesis_block.header.id,
                lib_slot: Slot::genesis(),
            }],
            then: StreamEnd::Hang,
        }]),
        immutable: vec![genesis_block],
        ..MockNode::default()
    };
    let mut sequencer =
        ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

    let mut finalized_items: Vec<FinalizedTx> = Vec::new();
    loop {
        match sequencer.next_event().await {
            Event::Ready => break,
            Event::BlocksProcessed { finalized, .. } => {
                assert!(
                    sequencer.channel_history().is_none(),
                    "finalized backfill does not know the live branch yet"
                );
                finalized_items.extend(finalized);
            }
            Event::TurnNotification { .. } => {}
        }
    }

    assert_eq!(
        finalized_items.len(),
        1,
        "expected exactly one finalized tx from genesis backfill"
    );
    let t = &finalized_items[0];
    assert_eq!(t.tx_hash, genesis_tx_hash);
    assert_eq!(t.ops.len(), 1);
    match &t.ops[0] {
        FinalizedOp::Inscription(info) => {
            assert_eq!(info.tx_hash, genesis_tx_hash);
            assert_eq!(info.parent_msg, MsgId::root());
            assert_eq!(info.this_msg, expected_msg_id);
        }
        other => panic!("expected Inscription, got {other:?}"),
    }
}

/// The finalized config tip must survive the real persistence path —
/// `build_checkpoint` → serde → `init_with_config` — not just the in-state
/// setter. A config-free resume must leave it intact, surfaced through the
/// checkpoint the sequencer re-emits.
#[tokio::test]
async fn finalized_config_survives_checkpoint_persistence_and_resume() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
    let finalized_config = MsgId::from([9; 32]);

    // `build_checkpoint` captures the finalized config tip.
    let mut state = TxState::new(header_id(0), MsgId::root());
    state.set_finalized_config(finalized_config);
    let cp = build_checkpoint(&state, MsgId::root(), Slot::genesis());
    assert_eq!(
        cp.finalized_config, finalized_config,
        "build_checkpoint saves it"
    );

    // It survives serde (and `serde(default)` does not clobber a set value).
    let json = serde_json::to_string(&cp).expect("serialize checkpoint");
    let cp: SequencerCheckpoint = serde_json::from_str(&json).expect("deserialize checkpoint");
    assert_eq!(
        cp.finalized_config, finalized_config,
        "serde round-trips it"
    );

    // `init_with_config` restores it; a config-free resume keeps it.
    let genesis_block = api_block(0, 0, 0, Vec::new());
    let live_block = api_block(1, 0, 1, Vec::new());
    let node = MockNode {
        lib: header_id(0),
        tip: header_id(0),
        immutable: vec![genesis_block],
        scripts: scripts(vec![StreamScript {
            events: vec![live_event(&live_block)],
            then: StreamEnd::Hang,
        }]),
        ..MockNode::default()
    };
    let mut sequencer =
        ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), Some(cp));

    let restored = loop {
        match sequencer.next_event().await {
            Event::BlocksProcessed { checkpoint, .. } => {
                break checkpoint.finalized_config;
            }
            Event::Ready | Event::TurnNotification { .. } => {}
        }
    };
    assert_eq!(
        restored, finalized_config,
        "resume restores finalized_config through the checkpoint"
    );
}

/// A config finalized during downtime, replayed by the resume backfill,
/// must refine `finalized_config` past the (stale) checkpoint seed —
/// exercising the backfill's config arm, not just the seed.
#[tokio::test]
async fn resume_backfill_refines_finalized_config_from_a_replayed_config() {
    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);

    // A pure config sits in the finalized history the backfill replays.
    let config_op = ChannelConfigOp {
        channel: channel_id,
        parent: MsgId::root(),
        keys: VerifiedChannelKeys::try_from(vec![Ed25519Key::from_bytes(&[0; 32]).public_key()])
            .unwrap(),
        posting_timeframe: SlotTimeframe::from(0u32),
        posting_timeout: SlotTimeout::from(0u32),
        configuration_threshold: 1,
        transfer_threshold: 1,
    };
    let config_id = config_op.id();
    let config_tx = unverified_tx_with_ops(vec![Op::ChannelConfig(config_op)]);
    // The config finalized during downtime at slot 1 — above the checkpoint
    // LIB (genesis), so the resume backfill must replay it.
    let config_block = api_block(1, 0, 1, vec![config_tx]);

    // The checkpoint's finalized_config is stale; the backfill must move it.
    let stale = MsgId::from([1; 32]);
    let mut state = TxState::new(header_id(0), MsgId::root());
    state.set_finalized_config(stale);
    let cp = build_checkpoint(&state, MsgId::root(), Slot::genesis());

    // A live block at slot 2 whose LIB is the config block (slot 1), so the
    // backfill catches up [genesis..=slot 1] and replays the config.
    let live_block = api_block(2, 1, 2, Vec::new());
    let event = ProcessedBlockEvent {
        block: live_block.clone(),
        tip: live_block.header.id,
        tip_slot: live_block.header.slot,
        lib: header_id(1),
        lib_slot: Slot::from(1),
    };
    let node = MockNode {
        lib: header_id(1),
        tip: header_id(1),
        immutable: vec![config_block],
        scripts: scripts(vec![StreamScript {
            events: vec![event],
            then: StreamEnd::Hang,
        }]),
        ..MockNode::default()
    };
    let mut sequencer =
        ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), Some(cp));

    let restored = loop {
        match sequencer.next_event().await {
            Event::BlocksProcessed { checkpoint, .. } => {
                break checkpoint.finalized_config;
            }
            Event::Ready | Event::TurnNotification { .. } => {}
        }
    };
    assert_eq!(
        restored, config_id,
        "backfill refines finalized_config to the replayed config"
    );
}

/// Realistic stream-gap scenario, driven end-to-end through the public
/// `ZoneSequencer` event loop.
///
/// Chain: G <- B1 <- B2 <- B3, one canonical branch, LIB at genesis.
/// The live stream delivers B1 (inscription A) and drops. B2 (inscription
/// Y, child of A) is mined during the outage. The stream resumes at B3;
/// the sequencer self-heals by backfilling B2.
///
/// Per the [`Event::Ready`] / [`ChannelUpdate`] contract, catch-up deltas
/// surface on the next `BlocksProcessed` once the stream resumes — so Y
/// must be reported as `adopted`. A consumer mirroring the channel from
/// `ChannelUpdate` otherwise silently misses Y until finalization.
#[tokio::test]
async fn stream_gap_surfaces_backfilled_inscriptions_as_adopted() {
    use std::time::Duration;

    use tokio::time::timeout;

    let channel_id = ChannelId::from([0; 32]);
    let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);

    let a = InscriptionOp {
        channel_id,
        parent: MsgId::root(),
        inscription: Inscription::new_unchecked(b"a".to_vec()),
        signer: sequencer_key.public_key().into_unverified(),
    };
    let a_id = a.id();
    let y = InscriptionOp {
        channel_id,
        parent: a_id,
        inscription: Inscription::new_unchecked(b"y".to_vec()),
        signer: sequencer_key.public_key().into_unverified(),
    };
    let y_id = y.id();

    let b1 = api_block(
        1,
        0,
        1,
        vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(a)])],
    );
    let b2 = api_block(
        2,
        1,
        2,
        vec![unverified_tx_with_ops(vec![Op::ChannelInscribe(y)])],
    );
    let b3 = api_block(3, 2, 3, Vec::new());

    let node = MockNode {
        scripts: scripts(vec![
            StreamScript {
                events: vec![live_event(&b1)],
                then: StreamEnd::End,
            },
            StreamScript {
                events: vec![live_event(&b3)],
                then: StreamEnd::Hang,
            },
        ]),
        blocks: vec![b2],
        ..MockNode::default()
    };
    let mut sequencer =
        ZoneSequencer::init(channel_id, sequencer_key, node, funding_config(), None);

    // Phase 1: drive until B1's channel update adopts A (Ready and turn
    // notifications interleave).
    let adopted = timeout(Duration::from_secs(10), async {
        loop {
            if let Event::BlocksProcessed { channel_update, .. } = sequencer.next_event().await
                && !channel_update.adopted().is_empty()
            {
                return channel_update.adopted().to_vec();
            }
        }
    })
    .await
    .expect("timed out waiting for B1's channel update");
    assert!(
        adopted
            .iter()
            .any(|t| t.inscription().is_some_and(|i| i.this_msg == a_id)),
        "sanity: A is adopted from the live B1"
    );

    // Phase 2: stream #1 has ended — a disconnect. `next_event`
    // reconnects internally and resumes at B3; the canonical backfill
    // fetches the missed B2. The first `BlocksProcessed` after the
    // reconnect is B3's ingestion and must carry Y as adopted.
    let update = timeout(Duration::from_secs(10), async {
        loop {
            if let Event::BlocksProcessed { channel_update, .. } = sequencer.next_event().await {
                return channel_update;
            }
        }
    })
    .await
    .expect("timed out waiting for the post-reconnect BlocksProcessed");
    assert!(
        update
            .adopted()
            .iter()
            .any(|t| t.inscription().is_some_and(|i| i.this_msg == y_id)),
        "inscription mined during the stream gap must surface as adopted on the next \
             BlocksProcessed after reconnect; got {update:?}",
    );
}

/// The variant follows `orphaned`: nothing orphaned is an extension,
/// otherwise a conflict whose prefix excludes the orphaned entries.
#[tokio::test]
async fn update_variant_follows_orphaned() {
    let channel_id = ChannelId::from([0; 32]);
    let key = Ed25519Key::from_bytes(&[0; 32]);
    let mut sequencer = ready_sequencer_with_channel(None, key.clone()).await;
    let entry = |n: u8| {
        let op = InscriptionOp {
            channel_id,
            inscription: Inscription::new_unchecked(vec![n]),
            parent: MsgId::root(),
            signer: key.public_key().into_unverified(),
        };
        let tx = unverified_tx_with_ops(vec![Op::ChannelInscribe(op.clone())]);
        ChannelUpdateTx::Inscription(InscriptionInfo {
            tx_hash: tx.hash(),
            parent_msg: MsgId::root(),
            this_msg: MsgId::root(),
            payload: op.inscription,
            signer: Some(op.signer),
        })
    };
    let result = |adopted: Vec<ChannelUpdateTx>, orphaned: Vec<ChannelUpdateTx>| BlockEventResult {
        finalized_items: Vec::new(),
        channel_update: Some(ChannelUpdateInfo {
            orphaned,
            adopted,
            new_channel_tip: MsgId::root(),
        }),
        common_prefix: vec![entry(1), entry(2)],
        deposits: Vec::new(),
    };

    let (update, ..) = sequencer.apply_block_result(result(vec![entry(3)], Vec::new()));
    assert!(matches!(update, ChannelUpdate::Extension { adopted } if adopted.len() == 1));

    let (update, ..) = sequencer.apply_block_result(result(vec![entry(3)], vec![entry(2)]));
    let ChannelUpdate::Conflict {
        common_prefix,
        adopted,
        orphaned,
    } = update
    else {
        panic!("an orphaned entry makes a conflict")
    };
    let hashes =
        |txs: &[ChannelUpdateTx]| txs.iter().map(ChannelUpdateTx::tx_hash).collect::<Vec<_>>();
    assert_eq!(hashes(&common_prefix), hashes(&[entry(1)]));
    assert_eq!(hashes(&adopted), hashes(&[entry(3)]));
    assert_eq!(hashes(&orphaned), hashes(&[entry(2)]));
}

async fn ready_sequencer_with_channel(
    channel: Option<ChannelState>,
    sequencer_key: Ed25519Key,
) -> ZoneSequencer<MockNode> {
    let (mut node, _posted_txs) = MockNode::with_posted_channel();
    node.channel_state = channel;
    let mut sequencer = ZoneSequencer::init(
        ChannelId::from([0; 32]),
        sequencer_key,
        node,
        funding_config(),
        None,
    );
    loop {
        if matches!(sequencer.next_event().await, Event::Ready) {
            break;
        }
    }
    sequencer
}

/// The config op's signature must claim our key's index in the *current*
/// accredited list — not index 0 — or the ledger rejects the update as
/// `InvalidSignature` whenever the sequencer is not the leading key.
#[tokio::test]
async fn channel_config_signs_with_own_current_accredited_index() {
    let own_key = Ed25519Key::from_bytes(&[7; 32]);
    let leading_key = Ed25519Key::from_bytes(&[0; 32]);
    let channel = ChannelState {
        accredited_keys: UnverifiedChannelKeys::new_unchecked(vec![
            leading_key.public_key().into_unverified(),
            own_key.public_key().into_unverified(),
        ])
        .into(),
        ..single_key_channel_state()
    };
    let mut sequencer = ready_sequencer_with_channel(Some(channel), own_key.clone()).await;

    let (_receipt, signed_ops) = sequencer
        .handle()
        .channel_config(
            VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
            SlotTimeframe::from(0u32),
            SlotTimeout::from(0u32),
            1,
            1,
        )
        .await
        .expect("config update from an accredited non-leading key must build");

    assert_eq!(
        config_op_of(&signed_ops).parent,
        MsgId::root(),
        "parent must equal the channel's config tip"
    );
    let OpProofRef::ChannelMultiSigProof(proof) =
        signed_ops.first().expect("config op proof present").proof()
    else {
        panic!("config op must carry a multi-sig proof");
    };
    let signatures = proof.signatures();
    assert_eq!(signatures.len(), 1);
    let (key_index, signature) = signatures
        .first_key_value()
        .expect("the proof holds one signature");
    assert_eq!(
        *key_index, 1,
        "signature must claim the signer's position in the current accredited list"
    );
    own_key
        .public_key()
        .into_unverified()
        .verify(signed_ops.hash().as_signing_bytes(), signature)
        .expect("signature must verify against the claimed key over the funded tx hash");
}

/// Configuring an unclaimed channel requires no signatures (the ledger
/// skips the check), so the proof must stay empty — a superfluous
/// signature would also break the node wallet's fee prediction.
#[tokio::test]
async fn channel_config_on_unclaimed_channel_carries_empty_proof() {
    let own_key = Ed25519Key::from_bytes(&[7; 32]);
    let mut sequencer = ready_sequencer_with_channel(None, own_key.clone()).await;

    let (_receipt, signed_ops) = sequencer
        .handle()
        .channel_config(
            VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
            SlotTimeframe::from(0u32),
            SlotTimeout::from(0u32),
            1,
            1,
        )
        .await
        .expect("claiming an unclaimed channel must build");

    let OpProofRef::ChannelMultiSigProof(proof) =
        signed_ops.first().expect("config op proof present").proof()
    else {
        panic!("config op must carry a multi-sig proof");
    };
    assert!(proof.signatures().is_empty());
    assert_eq!(
        config_op_of(&signed_ops).parent,
        MsgId::root(),
        "claiming an unclaimed channel must be rooted at ZERO"
    );
}

/// A sequencer whose key is not on the current accredited list cannot
/// produce a verifiable config signature; fail locally instead of
/// submitting a transaction that silently dies at block assembly.
#[tokio::test]
async fn channel_config_fails_when_not_accredited() {
    let own_key = Ed25519Key::from_bytes(&[7; 32]);
    let mut sequencer =
        ready_sequencer_with_channel(Some(single_key_channel_state()), own_key.clone()).await;

    let error = sequencer
        .handle()
        .channel_config(
            VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
            SlotTimeframe::from(0u32),
            SlotTimeout::from(0u32),
            1,
            1,
        )
        .await
        .expect_err("config update from a non-accredited key must fail locally");
    assert!(
        error.to_string().contains("accredited"),
        "unexpected error: {error}"
    );
}

/// `configuration_threshold > 1` needs signatures the sequencer cannot
/// collect; reject early with a clear error.
#[tokio::test]
async fn channel_config_rejects_multi_sig_threshold() {
    let own_key = Ed25519Key::from_bytes(&[7; 32]);
    let channel = ChannelState {
        accredited_keys: UnverifiedChannelKeys::new_unchecked(vec![
            own_key.public_key().into_unverified(),
            Ed25519Key::from_bytes(&[0; 32])
                .public_key()
                .into_unverified(),
        ])
        .into(),
        configuration_threshold: 2,
        ..single_key_channel_state()
    };
    let mut sequencer = ready_sequencer_with_channel(Some(channel), own_key.clone()).await;

    let error = sequencer
        .handle()
        .channel_config(
            VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
            SlotTimeframe::from(0u32),
            SlotTimeout::from(0u32),
            1,
            1,
        )
        .await
        .expect_err("multi-sig threshold config update must fail locally");
    assert!(
        error.to_string().contains("single-signer"),
        "unexpected error: {error}"
    );
}

/// A bundle carries its preparer's inscription signature, and the ledger
/// only accepts an inscription from the turn holder. Submitting another
/// sequencer's bundle through this runtime would post it in *our* turn
/// under *their* signature — unlandable. Reject at submit instead.
#[tokio::test]
async fn submit_atomic_bundle_rejects_a_bundle_prepared_by_another_sequencer() {
    use lb_core::{
        mantle::{
            ledger::{BoundedInputs, NoteId},
            transactions::MantleTxBuilder,
        },
        proofs::channel_multi_sig_proof::IndexedSignatures,
    };
    use lb_groth16::Fr;

    use super::super::types::{PreparedAtomicBundle, PreparedBundleKind};

    let own_key = Ed25519Key::from_bytes(&[7; 32]);
    let peer_key = Ed25519Key::from_bytes(&[8; 32]);
    let channel = ChannelState {
        accredited_keys: UnverifiedChannelKeys::new_unchecked(vec![
            own_key.public_key().into_unverified(),
            peer_key.public_key().into_unverified(),
        ])
        .into(),
        transfer_threshold: 2,
        ..single_key_channel_state()
    };
    let mut sequencer = ready_sequencer_with_channel(Some(channel), own_key.clone()).await;

    let sign_payload = vec![0xab; 32];
    let bundle_by = |preparer: &Ed25519Key| PreparedAtomicBundle {
        tx: Ops::new_unchecked(Vec::new()),
        transfer_proof: None,
        pre_fund: MantleTxBuilder::new(),
        inscribe_sig: preparer.sign_payload(&sign_payload),
        parent: MsgId::root(),
        msg_id: MsgId::root(),
        inscribe: Inscription::new_unchecked(b"pin".to_vec()),
        signer: preparer.public_key(),
        kind: PreparedBundleKind::PinDeposit {
            consumed_notes: BoundedInputs::from(NoteId::from(Fr::from(1u64))).into(),
        },
        sign_payload: sign_payload.clone(),
        accredited_keys: vec![
            own_key.public_key().into_unverified(),
            peer_key.public_key().into_unverified(),
        ],
        signing_threshold: 2,
    };

    // Prepared by the peer: refused before anything is tracked.
    let error = sequencer
        .handle()
        .submit_atomic_bundle(bundle_by(&peer_key), IndexedSignatures::empty())
        .expect_err("another sequencer's bundle must be refused");
    assert!(
        matches!(&error, Error::InvalidMultiSig(msg) if msg.contains("another sequencer")),
        "unexpected error: {error}"
    );
    assert!(
        sequencer
            .checkpoint()
            .is_none_or(|checkpoint| checkpoint.pending_txs.is_empty()),
        "refused bundle must not be tracked"
    );

    // Prepared by us: passes the ownership check and fails later, on the
    // (empty) multi-sig set — proving the check above is what fired.
    let error = sequencer
        .handle()
        .submit_atomic_bundle(bundle_by(&own_key), IndexedSignatures::empty())
        .expect_err("empty signature set under 2-of-2 must be refused");
    assert!(
        matches!(&error, Error::InvalidMultiSig(msg) if !msg.contains("another sequencer")),
        "unexpected error: {error}"
    );
}

fn config_op_of(tx: &SignedOps<Unverified, StandardMode>) -> ChannelConfigOp {
    tx.op_refs()
        .iter()
        .find_map(|op| match *op {
            OpRef::ChannelConfig(config) => Some(config.clone()),
            _ => None,
        })
        .expect("tx should carry a config op")
}

/// A configuration extends the mined config tip, never a config of ours
/// still in flight: the proof is built for the mined key set, so pairing
/// it with a pending config's id would produce a tx the ledger can only
/// reject. A config position has one pending continuation, so a second
/// config on the same mined tip is refused until the first lands or
/// expires.
#[tokio::test]
async fn a_second_config_on_a_pending_config_position_is_refused() {
    let own_key = Ed25519Key::from_bytes(&[7; 32]);
    let mut sequencer = ready_sequencer_with_channel(None, own_key.clone()).await;

    let (_receipt, first_tx) = sequencer
        .handle()
        .channel_config(
            VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
            SlotTimeframe::from(0u32),
            SlotTimeout::from(0u32),
            1,
            1,
        )
        .await
        .expect("first config should be accepted");
    let second = sequencer
        .handle()
        .channel_config(
            VerifiedChannelKeys::new_unchecked(vec![own_key.public_key()]),
            SlotTimeframe::from(1u32),
            SlotTimeout::from(0u32),
            1,
            1,
        )
        .await;

    assert_eq!(
        config_op_of(&first_tx).parent,
        MsgId::root(),
        "the config claiming an unclaimed channel must be rooted at ZERO"
    );
    assert!(
        matches!(second, Err(Error::ChannelStateChanged(_))),
        "the position is taken while the first config pends: {second:?}"
    );
}
