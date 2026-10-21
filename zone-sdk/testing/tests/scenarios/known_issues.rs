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

/// Known SDK issue (#3858): a backlog deeper than `max_pending_publish_depth`
/// stalls while LIB does not move. Messages queued out of turn are posted
/// by the resubmit loop, at most the depth at a time; entries mined above
/// LIB stay pending until finalized and count as in flight, so once one
/// window has landed nothing new is posted until LIB catches up, however
/// long that takes. On testnet-legacy it took twenty hours.
#[tokio::test(start_paused = true)]
#[ignore = "known issue: mined-but-unfinalized entries count against the publish depth (#3858)"]
async fn backlog_lands_while_lib_is_frozen() {
    let mut world = World::with_funding(chains::linear(), 100, 3000);
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    world.config_finalized(a, &["SEQ_B"], 3, 0).await;
    world.start("SEQ_B", reference("SEQ_B")).await;
    let depth = world.default_config().max_pending_publish_depth;
    world.wait_not_turn(a).await;
    let payloads: Vec<String> = (0..3 * depth).map(|i| format!("backlog-{i}")).collect();
    for payload in &payloads {
        world.publish(a, payload).await.expect("queued");
    }
    world.node().freeze_lib();
    let landed = world
        .step_until(20 * depth, |w| {
            payloads
                .iter()
                .all(|payload| w.is_payload_included(payload))
        })
        .await;
    let on_chain = payloads
        .iter()
        .filter(|payload| world.is_payload_included(payload))
        .count();
    assert!(
        landed,
        "only {on_chain} of {} landed under a frozen LIB:\n{}",
        payloads.len(),
        world.diagnostics()
    );
    world.node().thaw_lib();
    world.settle_chain(100).await;
    world.finish();
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
