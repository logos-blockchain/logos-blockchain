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
