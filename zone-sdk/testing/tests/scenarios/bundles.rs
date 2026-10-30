//! Multi-sig bundles and hand-built custom transactions.

use super::*;

/// S8: three independent sequencers run the 2-of-3 deposit lifecycle: the
/// in-turn sequencer prepares each bundle, two of them sign it, it submits.
pub async fn multisig_deposit_lifecycle(chain: Chain) {
    let mut world = World::new(chain);
    world.coin_split(&[5, 5, 5]).await;
    let recipient = world.funding_pk();
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    let b = world.start("SEQ_B", reference("SEQ_B")).await;
    world.start("SEQ_C", reference("SEQ_C")).await;

    escalate_config(&mut world, a, a, &["SEQ_A"], 1, 0, &[]).await;
    escalate_config(&mut world, a, a, &["SEQ_A", "SEQ_B"], 2, 0, &["SEQ_A"]).await;
    escalate_config(
        &mut world,
        a,
        b,
        &["SEQ_A", "SEQ_B", "SEQ_C"],
        2,
        15,
        &["SEQ_B", "SEQ_A"],
    )
    .await;

    let notes = world.take_user_notes(&[5]);
    let (hash, op, channel_notes) = world.deposit(&notes, "Mint 5 to channel").await;
    world.include(hash).await;
    let op_id = lb_core::mantle::ops::OpId::op_id(&op);
    let ids: Vec<NoteId> = channel_notes.iter().map(Utxo::id).collect();

    // A reorg to the finality depth can take the deposit's notes away for
    // a few blocks, and the SDK sheds a bundle whose inputs are gone; the
    // zone then runs the round again once the notes are back.
    let pin = payload_str(&pin_payload(&op_id));
    let pinned = land_bundle(
        &mut world,
        &pin,
        |world, index| {
            let ids = ids.clone();
            Box::pin(async move {
                let prepared = world
                    .harness
                    .seq(index)
                    .handle()
                    .prepare_pin_deposit(pin_payload(&op_id), ids)
                    .await
                    .expect("pin prepared");
                submit_bundle(world, index, prepared, &["SEQ_A", "SEQ_B"]).await;
            })
        },
        Some(&ids),
    )
    .await;
    assert!(pinned, "the pin never landed:\n{}", world.diagnostics());

    let withdraw = payload_str(&withdraw_payload(&op_id));
    let withdrawn = land_bundle(
        &mut world,
        &withdraw,
        |world, index| {
            Box::pin(async move {
                let prepared = world
                    .harness
                    .seq(index)
                    .handle()
                    .prepare_atomic_withdraw(
                        withdraw_payload(&op_id),
                        vec![WithdrawArg {
                            outputs: Outputs::new([Note::new(3, recipient)]),
                        }],
                        WithdrawInputs::Auto,
                    )
                    .await
                    .expect("withdraw prepared");
                submit_bundle(world, index, prepared, &["SEQ_B", "SEQ_C"]).await;
            })
        },
        None,
    )
    .await;
    assert!(
        withdrawn,
        "the withdraw never landed:\n{}",
        world.diagnostics()
    );
    world.settle_chain(400).await;

    let payloads = world.indexer_payloads().await;
    assert_eq!(
        count_payload(&payloads, &payload_str(&pin_payload(&op_id))),
        1,
        "pins"
    );
    assert_eq!(
        count_payload(&payloads, &payload_str(&withdraw_payload(&op_id))),
        1,
        "withdraws"
    );
    let wallet = world.wallet(a);
    assert_eq!(wallet.finalized.len(), 1, "finalized notes {wallet:?}");
    assert_eq!(wallet.unfinalized.len(), 0, "unfinalized notes {wallet:?}");
    assert!(wallet.finalized.iter().any(|n| n.value == 2), "{wallet:?}");
    world.finish();
}
scenario!(s08_multisig_deposit_lifecycle, multisig_deposit_lifecycle);

/// Two sequencers on a 2-of-2 channel whose first key, `SEQ_A`, holds the
/// turn for good, with one finalized deposit in `SEQ_A`'s wallet: the
/// world, `SEQ_A`'s index, the deposit's op id and its notes.
async fn two_of_two_with_deposit(
    chain: Chain,
    stale_refund_slots: Option<u64>,
) -> (World, usize, Hash, Vec<NoteId>) {
    let mut world = World::new(chain);
    world.coin_split(&[5]).await;
    let default = world.default_config();
    let config = SequencerConfig {
        stale_refund_slots: stale_refund_slots.unwrap_or(default.stale_refund_slots),
        ..default
    };
    let a = world.start_with("SEQ_A", config, passive()).await;
    world.start("SEQ_B", passive()).await;
    // The channel opens unkeyed: the first config needs no signature.
    escalate_config(&mut world, a, a, &["SEQ_A"], 1, 0, &[]).await;
    escalate_config(&mut world, a, a, &["SEQ_A", "SEQ_B"], 2, 0, &["SEQ_A"]).await;
    let notes = world.take_user_notes(&[5]);
    let (hash, op, channel_notes) = world.deposit(&notes, "Mint 5 to channel").await;
    world.include(hash).await;
    world.finalize_tx(hash).await;
    let op_id = lb_core::mantle::ops::OpId::op_id(&op);
    let ids: Vec<NoteId> = channel_notes.iter().map(Utxo::id).collect();
    wait_wallet_holds(&mut world, &ids, Some(a)).await;
    (world, a, op_id, ids)
}

/// Prepare the pin of `op_id`'s deposit on `index`.
async fn prepare_pin(
    world: &mut World,
    index: usize,
    op_id: &Hash,
    ids: &[NoteId],
) -> PreparedAtomicBundle {
    let prepared = world
        .harness
        .seq(index)
        .handle()
        .prepare_pin_deposit(pin_payload(op_id), ids.to_vec())
        .await
        .expect("pin prepared");
    world.harness.settle().await;
    prepared
}

/// Submit `prepared` with both signatures and step until it is finalized.
async fn land_pin(world: &mut World, index: usize, prepared: PreparedAtomicBundle, pin: &str) {
    let hash = submit_bundle(world, index, prepared, &["SEQ_A", "SEQ_B"]).await;
    world.include(hash).await;
    world.finalize_tx(hash).await;
    assert!(
        world.is_payload_finalized(pin),
        "the pin did not finalize:\n{}",
        world.diagnostics()
    );
    assert_eq!(
        count_payload(&world.indexer_payloads().await, pin),
        1,
        "the pin finalized more than once"
    );
}

/// S29: a bundle prepared on the channel tip is submitted after its
/// preparer published past it. The submit is refused as the channel moved,
/// nothing of the bundle stays pending, and the round run again on the
/// new tip lands.
pub async fn late_bundle_submit_is_refused_and_leaves_nothing_pending(chain: Chain) {
    let (mut world, a, op_id, ids) = two_of_two_with_deposit(chain, None).await;
    let pin = payload_str(&pin_payload(&op_id));
    let prepared = prepare_pin(&mut world, a, &op_id, &ids).await;
    world.publish_included(a, "takes the parent").await;

    let result = try_submit_bundle(&mut world, a, prepared, &["SEQ_A", "SEQ_B"]).await;
    assert!(
        matches!(result, Err(SdkError::ChannelStateChanged(_))),
        "a bundle on a moved channel was not refused as such: {result:?}"
    );
    assert!(
        !world.pending_payloads(a).contains(&pin),
        "the refused bundle stayed pending:\n{}",
        world.diagnostics()
    );

    let prepared = prepare_pin(&mut world, a, &op_id, &ids).await;
    land_pin(&mut world, a, prepared, &pin).await;
    world.finish();
}
scenario!(
    s29_late_bundle_submit_is_refused_and_leaves_nothing_pending,
    late_bundle_submit_is_refused_and_leaves_nothing_pending
);

/// S30: the channel's keys change between prepare and submit. The
/// signatures were collected under the old config, so the submit is refused
/// and the round is run again under the new one.
pub async fn bundle_prepared_under_an_old_config_is_refused(chain: Chain) {
    let (mut world, a, op_id, ids) = two_of_two_with_deposit(chain, None).await;
    let pin = payload_str(&pin_payload(&op_id));
    let prepared = prepare_pin(&mut world, a, &op_id, &ids).await;
    escalate_config(
        &mut world,
        a,
        a,
        &["SEQ_A", "SEQ_B", "SEQ_C"],
        2,
        0,
        &["SEQ_A", "SEQ_B"],
    )
    .await;

    let result = try_submit_bundle(&mut world, a, prepared, &["SEQ_A", "SEQ_B"]).await;
    assert!(
        matches!(result, Err(SdkError::ChannelStateChanged(_))),
        "a bundle under a stale config was not refused as such: {result:?}"
    );
    assert!(
        !world.pending_payloads(a).contains(&pin),
        "the refused bundle stayed pending:\n{}",
        world.diagnostics()
    );

    let prepared = prepare_pin(&mut world, a, &op_id, &ids).await;
    land_pin(&mut world, a, prepared, &pin).await;
    world.finish();
}
scenario!(
    s30_bundle_prepared_under_an_old_config_is_refused,
    bundle_prepared_under_an_old_config_is_refused
);

/// S31: one signature on a 2-of-2 channel. The submit is refused for its
/// signatures, nothing stays pending, and the same prepared bundle submitted
/// with both lands.
pub async fn undersigned_bundle_is_refused(chain: Chain) {
    let (mut world, a, op_id, ids) = two_of_two_with_deposit(chain, None).await;
    let pin = payload_str(&pin_payload(&op_id));
    let prepared = prepare_pin(&mut world, a, &op_id, &ids).await;

    let result = try_submit_bundle(&mut world, a, prepared.clone(), &["SEQ_A"]).await;
    assert!(
        matches!(result, Err(SdkError::InvalidMultiSig(_))),
        "an undersigned bundle was not refused as such: {result:?}"
    );
    assert!(
        !world.pending_payloads(a).contains(&pin),
        "the refused bundle stayed pending:\n{}",
        world.diagnostics()
    );

    land_pin(&mut world, a, prepared, &pin).await;
    world.finish();
}
scenario!(
    s31_undersigned_bundle_is_refused,
    undersigned_bundle_is_refused
);

/// S32: signatures take longer than the fee note's reservation while the
/// channel stays put. Nobody else drew the note, so the late submit is
/// taken and lands.
pub async fn late_bundle_submit_on_an_unmoved_channel_lands(chain: Chain) {
    let (mut world, a, op_id, ids) = two_of_two_with_deposit(chain, None).await;
    let pin = payload_str(&pin_payload(&op_id));
    let prepared = prepare_pin(&mut world, a, &op_id, &ids).await;
    world
        .steps(usize::try_from(PENDING_NOTE_EXPIRY_BLOCKS).expect("fits") + 2)
        .await;
    land_pin(&mut world, a, prepared, &pin).await;
    world.finish();
}
scenario!(
    s32_late_bundle_submit_on_an_unmoved_channel_lands,
    late_bundle_submit_on_an_unmoved_channel_lands
);

/// S22: two sequencers submit hand-built five-inscription txs, chained;
/// whatever a conflict drops is rebuilt on the new tip until every payload
/// is on chain.
pub async fn custom_multi_inscription(chain: Chain) {
    let mut world = World::with_funding(chain, 200, 3000);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 10, 0).await;
    world.stop(a);
    let funding_pk = world.funding_pk();
    let channel_id = world.channel_id();

    let sequencers = [("SEQ_A", "a"), ("SEQ_B", "b")];
    let mut batches: Vec<(usize, Vec<Inscription>)> = Vec::new();
    for (name, tag) in sequencers {
        let config = world.default_config();
        let index = world
            .start_as_with(name, seq_key(name), config, reference(name))
            .await;
        for tx in 0..5 {
            batches.push((
                index,
                (0..5)
                    .map(|entry| inscription(&format!("custom-{tag}-{tx}-{entry}")))
                    .collect(),
            ));
        }
    }
    let expected: Vec<String> = batches
        .iter()
        .flat_map(|(_, batch)| batch.iter().map(payload_str))
        .collect();

    for round in 0..10 {
        let on_chain: HashSet<String> = world.indexer_payloads().await.into_iter().collect();
        let mut parents: HashMap<usize, MsgId> = HashMap::new();
        let mut submitted = false;
        for (index, batch) in &batches {
            if batch.iter().all(|p| on_chain.contains(&payload_str(p))) {
                continue;
            }
            let parent = parents
                .get(index)
                .copied()
                .unwrap_or_else(|| world.view(*index).tip_message);
            let key = seq_key(&world.harness.seqs[*index].name);
            let (signed_tx, msg_id) = txs::build_funded_custom_tx(
                world.node(),
                channel_id,
                &key,
                funding_pk,
                batch,
                parent,
            )
            .await
            .expect("custom tx built");
            // The SDK may still be re-posting the previous tx on this
            // parent; then this round waits.
            if world
                .harness
                .seq(*index)
                .handle()
                .submit_signed_tx(signed_tx, msg_id)
                .is_err()
            {
                continue;
            }
            parents.insert(*index, msg_id);
            submitted = true;
        }
        if !submitted {
            break;
        }
        world.harness.settle().await;
        world.settle_chain(400).await;
        assert!(
            round < 9,
            "custom payloads kept dropping out:\n{}",
            world.diagnostics()
        );
    }

    let payloads = world.indexer_payloads().await;
    let found: HashSet<&str> = payloads.iter().map(String::as_str).collect();
    let missing: Vec<&String> = expected
        .iter()
        .filter(|p| !found.contains(p.as_str()))
        .collect();
    assert!(missing.is_empty(), "missing custom payloads: {missing:?}");
    world.finish();
}
scenario!(s22_custom_multi_inscription, custom_multi_inscription);

/// Run `submit` from the sequencer whose wallet holds `notes` (any running
/// one when empty) until the bundle carrying `payload` is finalized: again
/// whenever a reorg took the previous attempt away and the SDK shed it.
/// `false` when it never finalized.
async fn land_bundle(
    world: &mut World,
    payload: &str,
    submit: impl for<'a> Fn(
        &'a mut World,
        usize,
    ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    notes: Option<&[NoteId]>,
) -> bool {
    let max_steps = 3 * usize::try_from(common::FINALITY_DEPTH).expect("fits") + 10;
    // Only the sequencer that signed the bundle can land it again; the
    // others mirror it and re-post it, which the node refuses out of turn.
    let mut submitter = None;
    for _ in 0..4 {
        if world.is_payload_finalized(payload) {
            return true;
        }
        let held = submitter
            .is_some_and(|i: usize| world.pending_payloads(i).contains(&payload.to_owned()));
        if !world.is_payload_included(payload) && !held {
            let index = wait_wallet_holds(world, notes.unwrap_or(&[]), None).await;
            submit(world, index).await;
            submitter = Some(index);
        }
        let index = submitter.expect("submitted");
        let _ = world
            .step_until(max_steps, |w| {
                w.is_payload_finalized(payload)
                    || (!w.is_payload_included(payload)
                        && !w.pending_payloads(index).contains(&payload.to_owned()))
            })
            .await;
    }
    world.is_payload_finalized(payload)
}

/// A hand-built tx carries no funding of the SDK's own, so it cannot be
/// re-funded: priced out by a rise right after it was submitted, it is shed
/// once stale, reported orphaned and dropped from the pending set.
#[tokio::test(start_paused = true)]
async fn priced_out_custom_tx_is_shed_when_stale() {
    let mut world = World::with_funding(chains::linear(), 400, 300_000);
    let window = 3u64;
    let config = SequencerConfig {
        stale_refund_slots: window,
        ..world.default_config()
    };
    let a = world.start_with("SEQ_A", config, passive()).await;
    // Prices bind from the next block; funding reads them from the tip.
    world.set_gas_prices(100);
    world.extend(1, Posted::None).await;
    let first = world.publish_included(a, "opens the lineage").await;
    world.finalize_tx(first).await;
    let channel_id = world.channel_id();
    let parent = world
        .node()
        .channel_state_at_tip(channel_id)
        .expect("channel")
        .tip_message;
    let (signed_tx, msg_id) = txs::build_funded_custom_tx(
        world.node(),
        channel_id,
        &seq_key("SEQ_A"),
        world.funding_pk(),
        &[inscription("priced out")],
        parent,
    )
    .await
    .expect("custom tx built");
    let hash = signed_tx.hash();
    world
        .harness
        .seq(a)
        .handle()
        .submit_signed_tx(signed_tx, msg_id)
        .expect("submitted");
    world.set_gas_prices(200);
    world.harness.settle().await;
    let stamp = funding_stamp(&world, a, &hash).expect("stamped");

    let mut steps = 0;
    while lib_slot(&world) <= stamp + window {
        world.extend(1, Posted::All).await;
        world.harness.advance(World::STEP_DURATION).await;
        steps += 1;
        assert!(steps < 100, "LIB never passed the window");
    }
    let shed = world
        .step_until(10, |w| {
            !w.pending_payloads(a).contains(&"priced out".to_owned())
        })
        .await;
    assert!(
        shed,
        "the stale custom tx stayed pending:\n{}",
        world.diagnostics()
    );
    assert!(
        orphaned_hashes(&world, a).contains(&hash),
        "the shed custom tx was not reported orphaned"
    );
    assert!(!world.is_payload_included("priced out"));
    world.settle_chain(100).await;
    world.finish();
}

/// A 2-of-2 bundle needs its peer's signature over its hash, so the SDK
/// cannot re-fund it: stale, it is shed and reported orphaned, its stamp
/// never touched, and the round run again lands.
#[tokio::test(start_paused = true)]
async fn stale_multisig_bundle_is_shed_not_refunded() {
    let window = 3u64;
    let (mut world, a, op_id, ids) = two_of_two_with_deposit(chains::linear(), Some(window)).await;
    let pin = payload_str(&pin_payload(&op_id));
    let prepared = prepare_pin(&mut world, a, &op_id, &ids).await;
    let hash = submit_bundle(&mut world, a, prepared, &["SEQ_A", "SEQ_B"]).await;
    let stamp = funding_stamp(&world, a, &hash).expect("stamped");

    let mut steps = 0;
    while lib_slot(&world) <= stamp + window {
        world.extend(1, Posted::None).await;
        world.harness.advance(World::STEP_DURATION).await;
        if let Some(now) = funding_stamp(&world, a, &hash) {
            assert_eq!(now, stamp, "a 2-of-2 bundle was re-funded");
        }
        steps += 1;
        assert!(steps < 100, "LIB never passed the window");
    }
    for _ in 0..3 {
        world.extend(1, Posted::None).await;
        world
            .harness
            .advance(logos_blockchain_zone_sdk_testing::harness::RESUBMIT_INTERVAL)
            .await;
    }
    assert!(
        !world.pending_payloads(a).contains(&pin),
        "the stale bundle stayed pending:\n{}",
        world.diagnostics()
    );
    assert!(
        orphaned_hashes(&world, a).contains(&hash),
        "the shed bundle was not reported orphaned"
    );
    assert!(
        !fundings_by_payload(&world, a).contains_key(&pin),
        "the shed bundle was re-funded under a new hash"
    );

    // The node's mempool may still hold the shed bundle and mine it on the
    // next full block; otherwise the zone runs the round again. Either way
    // the pin lands once.
    world.settle_chain(100).await;
    if !world.is_payload_finalized(&pin) {
        let prepared = prepare_pin(&mut world, a, &op_id, &ids).await;
        land_pin(&mut world, a, prepared, &pin).await;
    }
    assert_eq!(
        count_payload(&world.indexer_payloads().await, &pin),
        1,
        "the pin did not finalize exactly once:\n{}",
        world.diagnostics()
    );
    world.finish();
}

/// A one-shot withdraw with automatic inputs beside an unmined pin leaves
/// the notes the pin consumes alone; both land once.
#[tokio::test(start_paused = true)]
async fn a_second_bundle_leaves_the_first_ones_notes_alone() {
    let mut world = World::new(chains::linear());
    world.coin_split(&[2, 2]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "opens").await;
    world.finalize_tx(first).await;
    let mut deposits: Vec<(Hash, Vec<NoteId>)> = Vec::new();
    for _ in 0..2 {
        let notes = world.take_user_notes(&[2]);
        let (hash, op, channel_notes) = world.deposit(&notes, "Mint 2").await;
        world.include(hash).await;
        world.finalize_tx(hash).await;
        deposits.push((
            lb_core::mantle::ops::OpId::op_id(&op),
            channel_notes.iter().map(Utxo::id).collect(),
        ));
    }
    let (pin_op, pin_ids) = deposits[0].clone();
    wait_wallet_holds(&mut world, &pin_ids, Some(a)).await;
    let (pinned, _) = world
        .harness
        .seq(a)
        .handle()
        .publish_pin_deposit(pin_payload(&pin_op), pin_ids.clone())
        .await
        .expect("pin accepted");
    world.harness.settle().await;
    assert!(!world.node().is_included(&pinned.inscription_id()));

    let withdraw = world
        .withdraw(a, 1, true, "withdraw beside the pin")
        .await
        .expect("withdraw accepted");
    let inputs = channel_inputs(&world, a, &withdraw);
    assert!(!inputs.is_empty(), "the withdraw spends no channel note");
    assert!(
        inputs.iter().all(|id| !pin_ids.contains(id)),
        "the withdraw spends a note the pending pin consumes: {inputs:?}\n{}",
        world.diagnostics()
    );
    world.settle_chain(200).await;
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        count_payload(&payloads, &payload_str(&pin_payload(&pin_op))),
        1,
        "pins"
    );
    assert_eq!(
        count_payload(&payloads, "withdraw beside the pin"),
        1,
        "withdraws"
    );
    world.finish();
}

/// A hand-built tx chained on a message whose next slot a pending publish
/// already holds is refused at submit, nothing of it pending.
#[tokio::test(start_paused = true)]
async fn hand_built_tx_on_a_parent_with_a_pending_child_is_refused() {
    let mut world = World::new(chains::linear());
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "first").await;
    world.publish(a, "second").await.expect("queued");
    let channel_id = world.channel_id();
    let (signed_tx, msg_id) = txs::build_funded_custom_tx(
        world.node(),
        channel_id,
        &seq_key("SEQ_A"),
        world.funding_pk(),
        &[inscription("on a taken parent")],
        message_of(&world, &first),
    )
    .await
    .expect("custom tx built");
    let result = world
        .harness
        .seq(a)
        .handle()
        .submit_signed_tx(signed_tx, msg_id);
    assert!(
        matches!(result, Err(SdkError::ChannelStateChanged(_))),
        "a tx on a parent with a pending child was not refused as such: {result:?}"
    );
    assert!(
        !world
            .pending_payloads(a)
            .contains(&"on a taken parent".to_owned()),
        "the refused tx stayed pending:\n{}",
        world.diagnostics()
    );
    world.settle_chain(100).await;
    world.finish();
}

/// A hand-built tx chained on a message whose next slot is already mined is
/// refused at submit the same way: the parent is taken on the chain itself.
#[tokio::test(start_paused = true)]
async fn hand_built_tx_on_a_parent_with_a_mined_child_is_refused() {
    let mut world = World::new(chains::linear());
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "first").await;
    world.publish_included(a, "second").await;
    let channel_id = world.channel_id();
    let (signed_tx, msg_id) = txs::build_funded_custom_tx(
        world.node(),
        channel_id,
        &seq_key("SEQ_A"),
        world.funding_pk(),
        &[inscription("behind the tip")],
        message_of(&world, &first),
    )
    .await
    .expect("custom tx built");
    let result = world
        .harness
        .seq(a)
        .handle()
        .submit_signed_tx(signed_tx, msg_id);
    assert!(
        matches!(result, Err(SdkError::ChannelStateChanged(_))),
        "a tx on a parent with a mined child was not refused as such: {result:?}"
    );
    assert!(
        !world
            .pending_payloads(a)
            .contains(&"behind the tip".to_owned()),
        "the refused tx stayed pending:\n{}",
        world.diagnostics()
    );
    world.settle_chain(100).await;
    world.finish();
}

/// A pin bundle pending at a restart comes back from the checkpoint as a
/// bundle, is posted again and lands once as a pin.
#[tokio::test(start_paused = true)]
async fn pending_pin_bundle_survives_a_restart_as_a_bundle() {
    let mut world = World::new(chains::linear());
    world.coin_split(&[2]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "opens").await;
    world.finalize_tx(first).await;
    let notes = world.take_user_notes(&[2]);
    let (hash, op, channel_notes) = world.deposit(&notes, "Mint 2").await;
    world.include(hash).await;
    world.finalize_tx(hash).await;
    let op_id = lb_core::mantle::ops::OpId::op_id(&op);
    let ids: Vec<NoteId> = channel_notes.iter().map(Utxo::id).collect();
    wait_wallet_holds(&mut world, &ids, Some(a)).await;
    let (pinned, _) = world
        .harness
        .seq(a)
        .handle()
        .publish_pin_deposit(pin_payload(&op_id), ids)
        .await
        .expect("pin accepted");
    let pin_hash = pinned.inscription_id();
    world.harness.settle().await;
    let checkpoint = world.checkpoint(a);
    assert!(
        checkpoint.pending_txs.iter().any(|(h, _)| *h == pin_hash),
        "the pin is not pending at the restart"
    );
    world.restart(a, Some(checkpoint)).await;
    world.include(pin_hash).await;
    world.settle_chain(100).await;
    let pin = payload_str(&pin_payload(&op_id));
    assert_eq!(
        count_payload(&world.indexer_payloads().await, &pin),
        1,
        "the restored pin did not land exactly once:\n{}",
        world.diagnostics()
    );
    world.finish();
}
