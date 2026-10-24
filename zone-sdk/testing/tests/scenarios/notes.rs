//! Channel notes: deposits, withdraws, pins, refunds and the wallet view.

use super::*;

/// S16: the channel wallet tracks deposited notes with exact values.
pub async fn channel_wallet_tracks_deposits(chain: Chain) {
    let mut world = World::new(chain);
    world.coin_split(&[1, 2]).await;
    let a = world.start("SEQ_A", passive()).await;
    world.publish_included(a, "initial inscription").await;

    let notes = world.take_user_notes(&[1, 2]);
    let (hash, _, _) = world.deposit(&notes, "Mint 1 and 2 into the channel").await;
    world.include(hash).await;
    let (finalized, unfinalized) = wallet_values(&world.wallet(a));
    let all: Vec<u64> = finalized
        .iter()
        .chain(unfinalized.iter())
        .copied()
        .collect();
    assert!(all.contains(&1) && all.contains(&2), "wallet {all:?}");

    world.finalize_tx(hash).await;
    let (finalized, unfinalized) = wallet_values(&world.wallet(a));
    assert!(
        finalized.contains(&1) && finalized.contains(&2),
        "{finalized:?}"
    );
    assert_eq!(finalized.len(), 2);
    assert!(unfinalized.is_empty(), "{unfinalized:?}");

    let checkpoint = world.checkpoint(a);
    world.restart(a, Some(checkpoint)).await;
    let (finalized, unfinalized) = wallet_values(&world.wallet(a));
    assert_eq!(
        (finalized.len(), unfinalized.len()),
        (2, 0),
        "after checkpoint restart"
    );

    world.restart(a, None).await;
    let (finalized, unfinalized) = wallet_values(&world.wallet(a));
    assert_eq!(
        (finalized.len(), unfinalized.len()),
        (2, 0),
        "after fresh restart"
    );
    world.finish();
}
scenario!(
    s16_channel_wallet_tracks_deposits,
    channel_wallet_tracks_deposits
);

/// S28: another zone's channel on the same chain is not ours. A deposit
/// into it stays out of the channel wallet, its inscriptions out of the
/// channel view, while a deposit into our channel beside it is tracked.
pub async fn other_channels_notes_are_not_ours(chain: Chain) {
    let mut world = World::new(chain);
    world.coin_split(&[1, 2, 3]).await;
    let a = world.start("SEQ_A", passive()).await;
    world.publish_included(a, "ours").await;

    // The other zone: a channel keyed to another sequencer, created by its
    // first inscription.
    let other_key = seq_key("SEQ_B");
    let other = lb_core::mantle::ops::channel::ChannelId::from(other_key.public_key().to_bytes());
    let (genesis, _) = txs::build_funded_custom_tx(
        world.node(),
        other,
        &other_key,
        world.funding_pk(),
        &[inscription("theirs")],
        MsgId::root(),
    )
    .await
    .expect("built");
    let genesis = world.submit_user_tx(genesis);
    world.include(genesis).await;

    let ours = world.take_user_notes(&[1]);
    let theirs = world.take_user_notes(&[2, 3]);
    let (ours, _, _) = world.deposit(&ours, "into ours").await;
    let (theirs, _) = txs::deposit_tx(world.node(), other, &theirs, "into theirs", world.user_pk())
        .await
        .expect("funded");
    let theirs = world.submit_user_tx(theirs);
    world.include(ours).await;
    world.include(theirs).await;
    world.finalize_tx(theirs).await;

    let (finalized, unfinalized) = wallet_values(&world.wallet(a));
    let all: Vec<u64> = finalized.iter().chain(&unfinalized).copied().collect();
    assert!(
        all.contains(&1),
        "our deposit is missing from the wallet: {all:?}"
    );
    assert!(
        !all.contains(&2) && !all.contains(&3),
        "the wallet holds the other channel's notes: {all:?}"
    );
    let payloads = world.indexer_payloads().await;
    assert_eq!(count_payload(&payloads, "theirs"), 0, "{payloads:?}");
    world.finish();
}
scenario!(
    s28_other_channels_notes_are_not_ours,
    other_channels_notes_are_not_ours
);

/// S17: finalized deposits are returned by the indexer.
pub async fn finalized_deposit_indexed(chain: Chain) {
    let mut world = World::new(chain);
    world.coin_split(&[1, 1, 1]).await;
    let a = world.start("SEQ_A", passive()).await;
    world.publish_included(a, "initial inscription").await;
    let notes = world.take_user_notes(&[1]);
    let (hash, op, _) = world.deposit(&notes, "Mint 1 to Alice in Zone").await;
    world.include(hash).await;
    world.finalize_tx(hash).await;
    let finalized = world.indexer_finalized().await;
    assert!(
        finalized_deposit(&finalized, &op, 1).is_some(),
        "deposit not indexed"
    );
    world.finish();
}
scenario!(s17_finalized_deposit_indexed, finalized_deposit_indexed);

/// S18: a deposit and its inscription are finalized together in one tx.
pub async fn atomic_deposit_and_inscription(chain: Chain) {
    let mut world = World::new(chain);
    world.coin_split(&[ATOMIC_DEPOSIT_FUNDING]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;

    // Like the cucumber step: the user's transfer creates the deposit note
    // and leaves a generous fee margin unspent.
    let utxo = world.take_user_notes(&[ATOMIC_DEPOSIT_FUNDING])[0];
    let user_pk = world.user_pk();
    let change = ATOMIC_DEPOSIT_FUNDING - 1 - ATOMIC_DEPOSIT_FEE_MARGIN;
    let transfer = TransferOp::new(
        Inputs::new(utxo.id()),
        Outputs::new([Note::new(1, user_pk), Note::new(change, user_pk)]),
    );
    let deposited = Utxo::new(transfer.op_id(), 0, Note::new(1, user_pk));
    let deposit = DepositOp {
        channel_id: world.channel_id(),
        inputs: Inputs::new(deposited.id()),
        metadata: b"Mint 1 to Alice in Zone"
            .to_vec()
            .try_into()
            .expect("metadata"),
    };
    let ops = Ops::try_from(vec![
        Op::Transfer(transfer),
        Op::ChannelDeposit(deposit.clone()),
    ])
    .expect("ops");
    let (ops, msg_id, signature) = world
        .harness
        .seq(a)
        .handle()
        .prepare_tx(ops, inscription("Mint 1 to Alice"))
        .expect("prepared");
    let proofs = OpProofs::try_from(vec![
        OpProof::ZkSig(ZkSignature::sample()),
        OpProof::ZkSig(ZkSignature::sample()),
        OpProof::Ed25519Sig(signature),
    ])
    .expect("proofs");
    let signed = SignedOps::from_parts(ops, proofs).expect("signed");
    let (result, _) = world
        .harness
        .seq(a)
        .handle()
        .submit_signed_tx(signed, msg_id)
        .expect("submitted");
    world.harness.settle().await;
    let hash = result.inscription_id();
    world.include(hash).await;
    world.finalize_tx(hash).await;

    let finalized = world.indexer_finalized().await;
    assert!(
        finalized_deposit(&finalized, &deposit, 1).is_some(),
        "deposit not indexed"
    );
    let payloads = common::inscription_payloads(&finalized);
    let expected = ["initial inscription", "Mint 1 to Alice"];
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(
    s18_atomic_deposit_and_inscription,
    atomic_deposit_and_inscription
);

/// S19: finalized withdraws reach both the indexer and the sequencer.
pub async fn finalized_withdraw(chain: Chain) {
    let mut world = World::new(chain);
    world.coin_split(&[3, 3, 3]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;
    let notes = world.take_user_notes(&[3]);
    let (hash, op, _) = world.deposit(&notes, "Mint 3 to Alice in Zone").await;
    world.include(hash).await;
    world.finalize_tx(hash).await;

    let user_pk = world.user_pk();
    // A failed fetch disconnects the sequencer for a moment; the zone retries.
    let mut attempts = 0;
    let result = loop {
        match world
            .harness
            .seq(a)
            .handle()
            .publish_atomic_withdraw(
                inscription("Burn 2"),
                vec![WithdrawArg {
                    outputs: Outputs::new([Note::new(2, user_pk)]),
                }],
                WithdrawInputs::Auto,
            )
            .await
        {
            Ok((result, _)) => break result,
            Err(error) => {
                attempts += 1;
                assert!(attempts < 20, "withdraw kept failing: {error}");
                world.harness.advance(RECONNECT_DELAY).await;
            }
        }
    };
    world.harness.settle().await;
    let PendingTx::AtomicWithdraw(info) = &result.tx else {
        panic!("expected an atomic withdraw, got {:?}", result.tx);
    };
    let withdraw_op = info.withdraws[0].op.clone();
    let withdraw_hash = result.inscription_id();
    world.include(withdraw_hash).await;
    world.finalize_tx(withdraw_hash).await;

    let finalized = world.indexer_finalized().await;
    assert!(
        finalized_deposit(&finalized, &op, 3).is_some(),
        "deposit not indexed"
    );
    let via_events = world.finalized_via_events(a);
    assert!(
        finalized_deposit(&via_events, &op, 3).is_some(),
        "deposit not seen by SEQ_A"
    );
    let has_withdraw = |txs: &[lb_zone_sdk::sequencer::FinalizedTx]| {
        txs.iter()
            .flat_map(|tx| tx.ops.iter())
            .any(|f| matches!(f, FinalizedOp::Withdraw(w) if w.op.inputs == withdraw_op.inputs))
    };
    assert!(has_withdraw(&finalized), "withdraw not indexed");
    assert!(has_withdraw(&via_events), "withdraw not seen by SEQ_A");
    let payloads = common::inscription_payloads(&finalized);
    let expected = ["initial inscription", "Burn 2"];
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(s19_finalized_withdraw, finalized_withdraw);

/// S20: an observed deposit is pinned, then the pinned note is withdrawn
/// before finalization.
pub async fn observed_deposit_pinned_and_withdrawn(chain: Chain) {
    let mut world = World::new(chain);
    world.coin_split(&[5, 5, 5]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;
    let recipient = world.funding_pk();
    let b = world
        .start_as("SEQ_B", seq_key("SEQ_A"), reference("SEQ_A"))
        .await;
    world.publish_included(a, "a1").await;
    world.publish_included(a, "a2").await;

    let notes = world.take_user_notes(&[5]);
    let (hash, op, channel_notes) = world.deposit(&notes, "Mint 5 for atomic integration").await;
    world.include(hash).await;
    let op_id = lb_core::mantle::ops::OpId::op_id(&op);
    let ids: Vec<NoteId> = channel_notes.iter().map(Utxo::id).collect();
    // A fork can orphan the deposit under the pin; the zone pins again
    // once the notes are back in its wallet.
    let pin = payload_str(&pin_payload(&op_id));
    let mut landed = false;
    for _ in 0..5 {
        wait_wallet_holds(&mut world, &ids, Some(b)).await;
        world
            .harness
            .seq(b)
            .handle()
            .publish_pin_deposit(pin_payload(&op_id), ids.clone())
            .await
            .expect("pin accepted");
        // The SDK re-posts the bundle itself while its inputs exist, under
        // a new hash once re-funded; only a shed bundle needs pinning again.
        let _ = world
            .step_until(60, |w| {
                w.is_payload_included(&pin) || !w.pending_payloads(b).contains(&pin)
            })
            .await;
        if world.is_payload_included(&pin) {
            landed = true;
            break;
        }
    }
    assert!(landed, "the pin never landed:\n{}", world.diagnostics());
    let outputs =
        Outputs::try_new([1, 1, 2].map(|v| Note::new(v, recipient)).to_vec()).expect("outputs");
    let (withdraw, _) = world
        .harness
        .seq(b)
        .handle()
        .publish_atomic_withdraw(
            withdraw_payload(&op_id),
            vec![WithdrawArg { outputs }],
            WithdrawInputs::Auto,
        )
        .await
        .expect("withdraw accepted");
    world.include(withdraw.inscription_id()).await;
    world.settle_chain(200).await;

    let (finalized, _) = wallet_values(&world.wallet(b));
    assert!(finalized.contains(&1), "SEQ_B wallet {finalized:?}");
    let indexed = world.indexer_finalized().await;
    assert!(
        finalized_transfer_input_counts(&indexed).contains(&1),
        "no finalized channel transfer consuming 1 input"
    );
    let payloads = common::inscription_payloads(&indexed);
    let expected = ["initial inscription", "a1", "a2"];
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(
    s20_observed_deposit_pinned_and_withdrawn,
    observed_deposit_pinned_and_withdrawn
);

/// S21: a withdrawal with automatic inputs sweeps the dust and stays within
/// 255 inputs.
pub async fn withdraw_under_dust_flood(chain: Chain) {
    let mut world = World::with_funding(chain, 60, 2000);
    world.coin_split(&[255, 10000]).await;
    let recipient = world.funding_pk();
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;

    let seed = world.take_user_notes(&[255]);
    let (hash, _, channel_notes) = world.deposit(&seed, "Dust seed").await;
    world.include(hash).await;
    assert!(
        world
            .step_until(40, |w| {
                let (f, u) = wallet_values(&w.wallet(a));
                f.contains(&255) || u.contains(&255)
            })
            .await,
        "seed note not in the wallet"
    );
    let user_pk = world.user_pk();
    let split = txs::build_channel_split_tx(
        world.node(),
        world.channel_id(),
        &seq_key("SEQ_A"),
        recipient,
        channel_notes[0],
        user_pk,
        255,
    )
    .await
    .expect("split built");
    let split_hash = lb_core::mantle::traits::Hashable::hash(&split);
    world.node().submit(split);
    world.include(split_hash).await;
    world.finalize_tx(split_hash).await;

    let big = world.take_user_notes(&[10000]);
    let (hash, op, channel_notes) = world.deposit(&big, "Big note").await;
    world.include(hash).await;
    let ids: Vec<NoteId> = channel_notes.iter().map(Utxo::id).collect();
    wait_wallet_holds(&mut world, &ids, Some(a)).await;
    let (withdraw, _) = world
        .harness
        .seq(a)
        .handle()
        .publish_atomic_withdraw(
            withdraw_payload(&lb_core::mantle::ops::OpId::op_id(&op)),
            vec![WithdrawArg {
                outputs: Outputs::new([Note::new(10000, recipient)]),
            }],
            WithdrawInputs::Auto,
        )
        .await
        .expect("withdraw accepted");
    world.include(withdraw.inscription_id()).await;
    world.settle_chain(200).await;

    let indexed = world.indexer_finalized().await;
    assert!(
        finalized_transfer_input_counts(&indexed).contains(&255),
        "no finalized channel transfer consuming 255 inputs: {:?}",
        finalized_transfer_input_counts(&indexed)
    );
    world.finish();
}
scenario!(s21_withdraw_under_dust_flood, withdraw_under_dust_flood);

/// S24: a reorg after a node restart double-hands a fee note; the loser is
/// re-funded and both inscriptions finalize once.
pub async fn refund_after_node_restart(chain: Chain) {
    let mut world = World::with_funding_values(chain, &[9000, 8000]);
    let config = SequencerConfig {
        stale_refund_slots: 4,
        ..world.default_config()
    };
    let a = world.start_with("SEQ_A", config, passive()).await;

    let first = world
        .publish(a, "message 1")
        .await
        .expect("publish")
        .inscription_id();
    world.include(first).await;
    let mined_in = world.node().block_of_tx(&first).expect("included");
    let fork_parent = world.node().block(mined_in).expect("block").parent;

    world.node().node_down();
    world.harness.settle().await;
    let tip_slot = world.node().tip_slot().into_inner();
    let mut parent = fork_parent;
    for i in 1..=2 {
        parent = world.node().mine_block(BlockSpec {
            parent: Some(parent),
            slot: Some((tip_slot + i).into()),
            txs: TxSelection::None,
            ..Default::default()
        });
    }
    assert!(
        !world.node().is_included(&first),
        "fork must drop message 1"
    );
    world.node().node_up();
    for _ in 0..10 {
        world.harness.advance(RECONNECT_DELAY).await;
        if world.node().open_streams() > 0 {
            break;
        }
    }
    let second = world.publish_retry(a, "message 2").await;
    let checkpoint = world.checkpoint(a);
    let first_notes = fee_notes(&checkpoint, &first);
    let second_notes = fee_notes(&checkpoint, &second);
    let overlap = first_notes.iter().any(|n| second_notes.contains(n));
    world.restart(a, Some(checkpoint)).await;
    world.settle_chain(200).await;

    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &["message 1", "message 2"]);
    assert!(
        overlap,
        "the two publishes did not share a fee note: {first_notes:?} vs {second_notes:?}"
    );
    let winners = [first, second]
        .iter()
        .filter(|h| world.node().is_tx_finalized(h))
        .count();
    assert_eq!(
        winners, 1,
        "exactly one double-handed tx finalizes under its submitted hash"
    );
    world.finish();
}
scenario!(s24_refund_after_node_restart, refund_after_node_restart);

/// Two sequencers withdraw against the same channel note; the loser's bundle
/// is pending with an input that no longer exists and must not wedge it.
pub async fn losing_withdraw_race_is_dropped(chain: Chain) {
    let mut world = World::new(chain);
    world.coin_split(&[2]).await;
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 3, 0).await;
    let b = world.start("SEQ_B", passive()).await;
    let notes = world.take_user_notes(&[2]);
    let (hash, _, _) = world.deposit(&notes, "shared note").await;
    world.include(hash).await;
    world.finalize_tx(hash).await;

    let user_pk = world.user_pk();
    let mut hashes = Vec::new();
    for (index, payload) in [(a, "A takes 1"), (b, "B takes 1")] {
        let (result, _) = world
            .harness
            .seq(index)
            .handle()
            .publish_atomic_withdraw(
                inscription(payload),
                vec![WithdrawArg {
                    outputs: Outputs::new([Note::new(1, user_pk)]),
                }],
                WithdrawInputs::Auto,
            )
            .await
            .expect("withdraw accepted");
        hashes.push(result.inscription_id());
        world.harness.settle().await;
    }
    // Whichever lands spends the note; the other can never be mined.
    world.steps(12).await;
    let landed = hashes
        .iter()
        .filter(|h| world.node().is_included(h))
        .count();
    assert_eq!(
        landed,
        1,
        "exactly one withdraw lands:\n{}",
        world.diagnostics()
    );

    // The loser must still be able to post afterwards.
    let loser = if world.node().is_included(&hashes[0]) {
        b
    } else {
        a
    };
    let after = world.publish_retry(loser, "after the race").await;
    world.include_within(after, 60).await;
    world.settle_chain(120).await;
    world.finish();
}
scenario!(
    s28_losing_withdraw_race_is_dropped,
    losing_withdraw_race_is_dropped
);
