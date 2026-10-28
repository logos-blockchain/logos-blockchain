//! Known SDK issues, each an ignored test with the issue in the reason;
//! they turn green when the fix lands.

use super::*;

/// A deposit mined only in a losing fork block is not reported in
/// `BlocksProcessed::deposits`: it never reached the canonical chain.
#[tokio::test(start_paused = true)]
async fn deposit_in_losing_fork_is_not_reported() {
    let mut world = World::new(chains::linear());
    world.coin_split(&[7]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;

    let notes = world.take_user_notes(&[7]);
    let (hash, _, _) = world.deposit(&notes, "fork-only deposit").await;
    // A competing block on the tip's parent takes the mempool but stays a
    // dead end: same height as the tip, so it never becomes canonical.
    let parent = world.node().ancestor_of_tip(1);
    let slot = world.node().tip_slot().into_inner() + 1;
    let fork = world.node().mine_block(BlockSpec {
        parent: Some(parent),
        slot: Some(slot.into()),
        ..Default::default()
    });
    world.harness.settle().await;
    assert!(!world.node().is_canonical_block(fork), "fork must lose");
    assert!(
        !world.node().is_included(&hash),
        "deposit must not be canonical"
    );
    world.steps(3).await;
    world.harness.assert_layers();
}

/// Known SDK issue: a publish queued out of turn on the channel tip is
/// stranded when another sequencer takes that parent while the sequencer
/// is down and the taking block finalizes before it is back. Restored from
/// the checkpoint, the pending entry no longer chains on the tip, so it is
/// never posted, never re-funded and, the displacement lying in finalized
/// history the restore replays without a shed, never reported orphaned:
/// the consumer is not told and the payload never lands.
#[tokio::test(start_paused = true)]
#[ignore = "known issue: a restored pending publish whose parent was taken by a block finalized while the sequencer was down stays pending, unreported and never posted"]
async fn publish_queued_out_of_turn_is_stranded_when_its_parent_is_taken_while_down() {
    let mut world = World::new(chains::linear());
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    world.config_finalized(a, &["SEQ_B"], 3, 0).await;
    let channel_id = world.channel_id();
    let state = world
        .node()
        .channel_state_at_tip(channel_id)
        .expect("channel");
    assert_eq!(
        state.tip_message,
        MsgId::root(),
        "a config-made channel starts at the root"
    );
    // Step until the turn is SEQ_B's, so SEQ_A's publish is queued unposted.
    let other = seq_key("SEQ_B");
    let other_pk = other.public_key().into_unverified();
    let in_turn = |world: &mut World| {
        let state = world
            .node()
            .channel_state_at_tip(channel_id)
            .expect("channel");
        let (index, _) = state.round_robin(world.node().tip_slot());
        state.accredited_keys.iter().nth(usize::from(index)) == Some(&other_pk)
    };
    assert!(
        world.step_until(6, in_turn).await,
        "SEQ_B never got the turn"
    );
    let queued = world
        .publish(a, "queued out of turn")
        .await
        .expect("queued")
        .inscription_id();
    assert!(
        !world.node().in_mempool(&queued),
        "must be queued, not posted"
    );
    let checkpoint = world.checkpoint(a);
    world.stop(a);

    // SEQ_B takes the root while SEQ_A is down.
    let (taken, _) = txs::build_funded_custom_tx(
        world.node(),
        channel_id,
        &other,
        world.funding_pk(),
        &[inscription("taken by SEQ_B")],
        MsgId::root(),
    )
    .await
    .expect("built");
    let taken_hash = taken.hash();
    let slot = world.node().tip_slot().into_inner() + 1;
    let _ = world.node().mine_block(BlockSpec {
        slot: Some(slot.into()),
        txs: TxSelection::None,
        extra_txs: vec![taken],
        ..Default::default()
    });
    world.harness.settle().await;
    // The taking block finalizes before SEQ_A comes back.
    world.finalize_tx(taken_hash).await;
    world.restart(a, Some(checkpoint)).await;

    // Displaced: reported orphaned, republished by the consumer on the new
    // tip, and landed.
    world.settle_chain(100).await;
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        count_payload(&payloads, "queued out of turn"),
        1,
        "the displaced publish never landed: {payloads:?}\n{}",
        world.diagnostics()
    );
    world.finish();
}

/// Known SDK issue: another sequencer's config mined on a branch the chain
/// abandons is never reported orphaned. The config joins the view when its
/// block is canonical; when a longer fork from below it wins, the sequencer
/// keeps it as a pending entry chained on the config tip and reports no
/// change, never re-posts it and, since it is another sequencer's, never
/// sheds it: the view carries it for good.
#[tokio::test(start_paused = true)]
#[ignore = "known issue: another sequencer's config on an abandoned branch stays pending, unreported, never re-posted"]
async fn others_config_on_an_abandoned_branch_is_not_reported_orphaned() {
    let mut world = World::new(chains::linear());
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 3, 0).await;
    let channel_id = world.channel_id();
    let state = world
        .node()
        .channel_state_at_tip(channel_id)
        .expect("channel");
    let key = seq_key("SEQ_B");
    let index = state
        .accredited_keys
        .iter()
        .position(|k| *k == key.public_key().into_unverified())
        .expect("SEQ_B accredited");
    let config = txs::build_channel_config_tx(
        world.node(),
        channel_id,
        txs::Signer {
            key: &key,
            index: u16::try_from(index).expect("fits"),
        },
        world.funding_pk(),
        state.config_tip_hash,
        txs::ConfigRules {
            keys: keys(&["SEQ_A", "SEQ_B"]),
            posting_timeframe: 4,
            posting_timeout: 0,
            configuration_threshold: 1,
            transfer_threshold: 1,
        },
    )
    .await
    .expect("built");
    let config_hash = config.hash();
    let mine = |world: &World, parent: lb_core::header::HeaderId, slot: u64, extra| {
        world.node().mine_block(BlockSpec {
            parent: Some(parent),
            slot: Some(slot.into()),
            txs: TxSelection::None,
            extra_txs: extra,
            ..Default::default()
        })
    };

    // The config lands and joins the view.
    let fork_parent = world.node().tip();
    let slot = world.node().tip_slot().into_inner();
    mine(&world, fork_parent, slot + 1, vec![config]);
    world.harness.settle().await;
    assert!(
        world.node().is_included(&config_hash),
        "the config must land"
    );

    // A longer fork from below it wins.
    let first = mine(&world, fork_parent, slot + 2, vec![]);
    let winner = mine(&world, first, slot + 3, vec![]);
    world.harness.settle().await;
    assert_eq!(world.node().tip(), winner, "the fork must win");
    assert!(!world.node().is_included(&config_hash));
    world.steps(3).await;

    // Reported orphaned, the config leaves the pending set and the view.
    let pending: Vec<_> = world
        .checkpoint(a)
        .pending_txs
        .iter()
        .map(|(hash, _)| *hash)
        .collect();
    assert!(
        !pending.contains(&config_hash),
        "the orphaned config is still pending: {pending:?}"
    );
    world.harness.assert_layers();
}

/// Known SDK issue: a writer's inscription and the two-inscription tx chained
/// on it are mined, then reorged out while a price rise keeps the writer from
/// re-landing them, so both sit in the pending tail when the writer's config
/// lands. The config shed drops the inscription but not the custom tx chained
/// on it, which leaves the view without being reported orphaned.
#[tokio::test(start_paused = true)]
#[ignore = "known issue: the config shed drops a pending inscription but not the custom tx chained on it"]
async fn config_shed_strands_the_custom_tx_chained_on_a_shed_inscription() {
    let mut world = World::new(chains::linear());
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &[FOREIGN], 1, 0).await;
    world.add_writer_with(
        FOREIGN,
        &[WriterOp::Inscribe, WriterOp::InscribeTwo, WriterOp::Config],
    );
    world.enable_foreign_traffic(2);
    let mined = world
        .step_until(12, |w| {
            w.writers[0].posted.len() == 2
                && w.writers[0]
                    .posted_hashes()
                    .all(|h| w.node().is_included(&h))
        })
        .await;
    assert!(
        mined,
        "the writer's inscription and custom tx were not mined:\n{}",
        world.diagnostics()
    );

    // Too dear to re-land: both stay in the pending tail of the new branch.
    world.node().set_gas_prices(GasPrices::new(2, 2));
    world.fork(3).await;
    world.enable_foreign_traffic(1);
    world.steps(2).await;
    let config = world.writers[0].posted_hashes().nth(2);
    assert!(
        config.is_some_and(|hash| world.node().is_included(&hash)),
        "the writer's config did not land:\n{}",
        world.diagnostics()
    );
    world.harness.assert_layers();
}

/// A withdraw spends the note of an unfinalized deposit; a fork drops the
/// deposit and the user spends the same input again, so the channel note
/// never exists. The bundle's lineage parent stays valid, so only its input
/// is dead: it must be shed, not resubmitted forever, and must not block
/// later publishes.
#[tokio::test(start_paused = true)]
#[ignore = "known issue: a pending withdraw whose channel input vanished with an orphaned deposit is resubmitted forever"]
async fn withdraw_on_orphaned_deposit_is_dropped() {
    let mut world = World::new(chains::linear());
    world.coin_split(&[2]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;

    let notes = world.take_user_notes(&[2]);
    let (deposit, _, _) = world.deposit(&notes, "first deposit").await;
    world.include(deposit).await;
    let unfinalized: Vec<u64> = world
        .wallet(a)
        .unfinalized
        .iter()
        .map(|n| n.value)
        .collect();
    assert_eq!(unfinalized, vec![2], "the deposit is in the wallet");

    let user_pk = world.user_pk();
    let (result, _) = world
        .harness
        .seq(a)
        .handle()
        .publish_atomic_withdraw(
            inscription("withdraw 1"),
            vec![WithdrawArg {
                outputs: Outputs::new([Note::new(1, user_pk)]),
            }],
            WithdrawInputs::Auto,
        )
        .await
        .expect("withdraw accepted");
    let withdraw = result.inscription_id();
    world.harness.settle().await;

    // Fork over the deposit's block; the fork spends the user's note in a
    // second deposit, so the first deposit can never return.
    let (second, _) = txs::deposit_tx(
        world.node(),
        world.channel_id(),
        &notes,
        "second deposit",
        user_pk,
    )
    .await
    .expect("second deposit funded");
    let parent = world.node().ancestor_of_tip(1);
    let tip_slot = world.node().tip_slot().into_inner();
    let fork = world.node().mine_block(BlockSpec {
        parent: Some(parent),
        slot: Some((tip_slot + 1).into()),
        txs: TxSelection::None,
        extra_txs: vec![second],
        ..BlockSpec::default()
    });
    world.harness.settle().await;
    let _ = world.node().mine_block(BlockSpec {
        parent: Some(fork),
        slot: Some((tip_slot + 2).into()),
        txs: TxSelection::Except(vec![deposit, withdraw]),
        ..BlockSpec::default()
    });
    world.harness.settle().await;
    world.steps(3).await;
    assert!(
        !world.node().is_included(&deposit),
        "the first deposit is dead"
    );

    // The bundle's input never exists again; the sequencer must let it go
    // and keep publishing.
    world.steps(40).await;
    assert!(!world.node().is_included(&withdraw));
    let after = world.publish_retry(a, "after the dead withdraw").await;
    world.include_within(after, 60).await;
    assert_eq!(
        world.pending_count(a),
        0,
        "SEQ_A still holds the dead withdraw:\n{}",
        world.diagnostics()
    );
    world.settle_chain(120).await;
    world.finish();
}

/// Known SDK issue: a hand-built tx chained on a message whose next slot is
/// finalized is accepted at submit. The only parent check is against the
/// pending children, own or mirrored, and a finalized child has left them;
/// the bundle path also compares the parent with the view tip. The entry
/// pends on a passed parent, is re-posted and refused by the node, and is
/// not reported orphaned until the expiry shed.
#[tokio::test(start_paused = true)]
#[ignore = "known issue: submit_signed_tx accepts a parent whose taking child is finalized; only pending children are checked"]
async fn hand_built_tx_on_a_parent_with_a_finalized_child_is_refused() {
    let mut world = World::new(chains::linear());
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "first").await;
    let second = world.publish_included(a, "second").await;
    world.finalize_tx(second).await;
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
        "a tx on a parent with a finalized child was not refused: {result:?}"
    );
    assert!(
        !world
            .pending_payloads(a)
            .contains(&"behind the tip".to_owned()),
        "the accepted tx stayed pending:\n{}",
        world.diagnostics()
    );
    world.settle_chain(100).await;
    world.finish();
}
