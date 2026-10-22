//! Publishing: messages, checkpoints, same-key instances, turns and a
//! foreign writer.

use super::*;

/// S1: publish messages, read them back from the indexer, add a config.
pub async fn publish_and_index(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", passive()).await;

    let mut hashes = Vec::new();
    for payload in ["Hello, Zone!", "Second message", "Third message"] {
        hashes.push(world.publish_included(a, payload).await);
    }
    let config_keys = keys(&["SEQ_A", "SEQ_B"]);
    let (config, _) = world
        .harness
        .seq(a)
        .handle()
        .channel_config(config_keys, 0.into(), 0.into(), 1, 1)
        .await
        .expect("config accepted");
    let config_hash = config.0.inscription_id();
    world.harness.settle().await;

    world.finalize_all(&hashes).await;
    world.include(config_hash).await;
    world.finalize_tx(config_hash).await;

    let payloads = world.indexer_payloads().await;
    let expected = ["Hello, Zone!", "Second message", "Third message"];
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(s01_publish_and_index, publish_and_index);

/// S2: resume from a checkpoint and keep publishing.
pub async fn resume_from_checkpoint(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", passive()).await;
    let mut hashes = vec![
        world.publish_included(a, "Message 1").await,
        world.publish_included(a, "Message 2").await,
    ];
    let checkpoint = world.checkpoint(a);
    world.restart(a, Some(checkpoint)).await;
    hashes.push(world.publish_included(a, "Message 3").await);
    hashes.push(world.publish_included(a, "Message 4").await);
    world.finalize_all(&hashes).await;

    let expected = ["Message 1", "Message 2", "Message 3", "Message 4"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(s02_resume_from_checkpoint, resume_from_checkpoint);

/// S3: a stale checkpoint must not re-post what already finalized.
pub async fn resume_from_stale_checkpoint(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", passive()).await;
    let hashes = vec![
        world.publish_included(a, "msg-1").await,
        world.publish_included(a, "msg-2").await,
    ];
    let stale = world.checkpoint(a);
    world.finalize_all(&hashes).await;

    world.restart(a, None).await;
    world.steps(2).await;
    let hashes = vec![
        world.publish_included(a, "msg-3").await,
        world.publish_included(a, "msg-4").await,
    ];
    world.finalize_all(&hashes).await;

    world.restart(a, Some(stale)).await;
    world.steps(2).await;
    let hash = world.publish_included(a, "msg-5").await;
    world.finalize_tx(hash).await;
    world.settle_chain(40).await;

    let expected = ["msg-1", "msg-2", "msg-3", "msg-4", "msg-5"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(
    s03_resume_from_stale_checkpoint,
    resume_from_stale_checkpoint
);

/// S4: publishes fail fast while the node is down and work after reconnect.
pub async fn publish_while_node_down(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", passive()).await;

    world.node().node_down();
    world.harness.settle().await;
    assert!(
        world.publish(a, "while down").await.is_err(),
        "publish must fail while the node is down"
    );
    world.node().node_up();

    let mut hashes = Vec::new();
    for payload in ["After down (1)", "After down (2)", "After down (3)"] {
        let mut result = None;
        for _ in 0..20 {
            match world.publish(a, payload).await {
                Ok(r) => {
                    result = Some(r);
                    break;
                }
                Err(_) => world.harness.advance(RECONNECT_DELAY).await,
            }
        }
        let hash = result
            .expect("publish accepted after reconnect")
            .inscription_id();
        world.include(hash).await;
        hashes.push(hash);
    }
    world.finalize_all(&hashes).await;

    let expected = ["After down (1)", "After down (2)", "After down (3)"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(s04_publish_while_node_down, publish_while_node_down);

/// S5: two instances of the same key publish one after the other.
pub async fn sequential_same_key(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", passive()).await;
    let mut hashes = Vec::new();
    for payload in ["a1", "a2", "a3"] {
        hashes.push(world.publish_included(a, payload).await);
    }
    world.finalize_all(&hashes).await;
    world.stop(a);

    let b = world.start_as("SEQ_B", seq_key("SEQ_A"), passive()).await;
    let mut hashes = Vec::new();
    for payload in ["b1", "b2", "b3"] {
        hashes.push(world.publish_included(b, payload).await);
    }
    world.finalize_all(&hashes).await;
    world.stop(b);

    world.restart(a, None).await;
    let mut hashes = Vec::new();
    for payload in ["a4", "a5", "a6"] {
        hashes.push(world.publish_included(a, payload).await);
    }
    world.finalize_all(&hashes).await;

    let expected = ["a1", "a2", "a3", "b1", "b2", "b3", "a4", "a5", "a6"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(s05_sequential_same_key, sequential_same_key);

/// S6: three same-key writers race 20 messages each and converge.
pub async fn concurrent_same_key(chain: Chain) {
    let mut world = World::with_funding(chain, 200, 3000);
    let a = world.start("SEQ_A", passive()).await;
    world.stop(a);

    let mut expected = Vec::new();
    for (name, prefix) in [("SEQ_A", "a"), ("SEQ_B", "b"), ("SEQ_C", "c")] {
        let planned: Vec<String> = (1..=20).map(|i| format!("{prefix}{i}")).collect();
        expected.extend(planned.clone());
        let config = world.config_with_depth(usize::MAX);
        let planned = planned.iter().map(|p| inscription(p)).collect();
        let policy = ReferenceConsumer::boxed(&seq_key("SEQ_A"), planned);
        world
            .start_as_with(name, seq_key("SEQ_A"), config, policy)
            .await;
    }
    world.settle_chain(300).await;

    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &expected);
    world.finish();
}
scenario!(s06_concurrent_same_key, concurrent_same_key);

/// S9: a depth-limited sequencer waits for its turn and submits pending,
/// the next window once the last one has landed.
pub async fn round_robin_depth_two(chain: Chain) {
    round_robin_queue(
        chain,
        2,
        ["rr-queued-b1", "rr-queued-b2", "rr-queued-b3"],
        false,
    )
    .await;
}
scenario!(s09_round_robin_depth_two, round_robin_depth_two);

/// S10: with no depth limit every pending message is submitted on turn.
pub async fn round_robin_unlimited(chain: Chain) {
    round_robin_queue(
        chain,
        usize::MAX,
        ["rr-unbounded-1", "rr-unbounded-2", "rr-unbounded-3"],
        true,
    )
    .await;
}
scenario!(s10_round_robin_unlimited, round_robin_unlimited);

/// S12: a sequencer publishes immediately when it is its turn.
pub async fn publish_in_turn(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    let b = world.start("SEQ_B", passive()).await;

    world.wait_turn(a).await;
    world.wait_turn(b).await;
    world.wait_turn(a).await;
    let hash = world
        .publish(a, "decentralized-immediate-publish")
        .await
        .expect("publish")
        .inscription_id();
    world.include(hash).await;
    world.finalize_tx(hash).await;
    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &["decentralized-immediate-publish"]);
    world.finish();
}
scenario!(s12_publish_in_turn, publish_in_turn);

/// S13: sequencers are added to the rotation one config at a time.
pub async fn dynamic_sequencers(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    world.config_finalized(a, &[], 2, 0).await;
    world.wait_turn(a).await;
    world.publish(a, "seq_a-msg1").await.expect("publish");

    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    let b = world.start("SEQ_B", reference("SEQ_B")).await;
    world.wait_turn(b).await;
    world.publish(b, "seq_b-msg1").await.expect("publish");

    world.config_finalized(a, &["SEQ_B", "SEQ_C"], 2, 0).await;
    let c = world.start("SEQ_C", reference("SEQ_C")).await;
    world.wait_turn(c).await;
    world.publish(c, "seq_c-msg1").await.expect("publish");

    world.publish(a, "seq_a-msg2").await.expect("publish");
    world.publish(b, "seq_b-msg2").await.expect("publish");
    world.publish(c, "seq_c-msg2").await.expect("publish");
    world.settle_chain(200).await;

    let expected = [
        "seq_a-msg1",
        "seq_b-msg1",
        "seq_c-msg1",
        "seq_a-msg2",
        "seq_b-msg2",
        "seq_c-msg2",
    ];
    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &expected);
    world.finish();
}
scenario!(s13_dynamic_sequencers, dynamic_sequencers);

/// S15: identical payloads published concurrently each land exactly once.
pub async fn identical_payloads(chain: Chain) {
    let mut world = World::with_funding(chain, 200, 3000);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B", "SEQ_C"], 60, 0).await;
    world.stop(a);
    for name in ["SEQ_A", "SEQ_B", "SEQ_C"] {
        let config = world.config_with_depth(usize::MAX);
        let planned = vec![inscription("shared-message"); 10];
        world
            .start_as_with(
                name,
                seq_key(name),
                config,
                ReferenceConsumer::boxed(&seq_key(name), planned),
            )
            .await;
    }
    world.steps(190).await;
    world.settle_chain(200).await;

    let payloads = world.indexer_payloads().await;
    let copies = payloads
        .iter()
        .filter(|p| p.as_str() == "shared-message")
        .count();
    assert_eq!(copies, 30, "copies of the shared message");
    world.finish();
}
scenario!(s15_identical_payloads, identical_payloads);

/// S27: an accredited key driven outside the SDK posts in its own turns; the
/// SDK sequencer adopts those entries and keeps chaining after them.
pub async fn foreign_sequencer_writes(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    world.config_finalized(a, &["SEQ_C"], 3, 0).await;
    let foreign_key = seq_key("SEQ_C");
    let foreign_index = 1u16;
    let funding_pk = world.funding_pk();
    let channel_id = world.channel_id();

    let ours = ["a0", "a1", "a2"];
    let theirs = ["x0", "x1", "x2"];
    for payload in ours {
        world.publish(a, payload).await.expect("queued");
    }
    let mut queue: Vec<&str> = theirs.to_vec();
    let mut in_flight: Option<(lb_zone_sdk::node_types::TxHash, &str)> = None;
    for _ in 0..120 {
        if let Some((hash, payload)) = in_flight {
            let node = world.node();
            let in_mempool = |n: &logos_blockchain_zone_sdk_testing::SimNode| {
                n.mempool().iter().any(|tx| tx.hash() == hash)
            };
            let known_to_peer = world
                .peer
                .as_ref()
                .is_some_and(|peer| peer.is_included(&hash) || in_mempool(peer));
            // Tracked until finalized: a fork can still orphan what landed.
            if node.is_tx_finalized(&hash) {
                in_flight = None;
            } else if !node.is_included(&hash) && !in_mempool(node) && !known_to_peer {
                // Dropped everywhere (a fork or a lost turn): post it again.
                queue.insert(0, payload);
                in_flight = None;
            }
        }
        if in_flight.is_none() && !queue.is_empty() {
            let node = world.node();
            let state = node.channel_state_at_tip(channel_id).expect("channel");
            let next_slot = node.tip_slot().into_inner() + 1;
            if state.round_robin(next_slot.into()).0 == foreign_index {
                let payload = queue.remove(0);
                let (tx, _) = txs::build_funded_custom_tx(
                    node,
                    channel_id,
                    &foreign_key,
                    funding_pk,
                    &[inscription(payload)],
                    state.tip_message,
                )
                .await
                .expect("foreign tx built");
                let hash = tx.hash();
                node.submit(tx);
                in_flight = Some((hash, payload));
            }
        }
        if queue.is_empty() && in_flight.is_none() && world.pending_count(a) == 0 {
            break;
        }
        world.step().await;
    }
    world.settle_chain(200).await;

    let payloads = world.indexer_payloads().await;
    let mut expected: Vec<&str> = ours.to_vec();
    expected.extend(theirs);
    assert_exactly_once_any_order(&payloads, &expected);
    let adopted: HashSet<String> = world.harness.seqs[a]
        .events
        .iter()
        .filter_map(|e| match e {
            lb_zone_sdk::sequencer::Event::BlocksProcessed { channel_update, .. } => {
                Some(channel_update)
            }
            _ => None,
        })
        .flat_map(|update| update.adopted().iter())
        .filter_map(|entry| entry.inscription())
        .map(|info| payload_str(&info.payload))
        .collect();
    for payload in theirs {
        assert!(adopted.contains(payload), "SEQ_A never adopted {payload}");
    }
    world.finish();
}
scenario!(s27_foreign_sequencer_writes, foreign_sequencer_writes);

/// A backlog deeper than `max_pending_publish_depth` keeps landing while
/// LIB does not move. Messages queued out of turn are posted by the
/// resubmit loop, at most the depth at a time; entries mined above LIB
/// stay pending until finalized but no longer count as in flight (#3858),
/// so the next window goes out as soon as the last one landed. Before the
/// fix only one window landed until LIB caught up; on testnet-legacy that
/// took twenty hours.
#[tokio::test(start_paused = true)]
async fn backlog_lands_while_lib_is_frozen() {
    let mut world = World::with_funding(chains::linear(), 100, 3000);
    let depth = 10;
    let config = world.config_with_depth(depth);
    let a = world.start_with("SEQ_A", config, reference("SEQ_A")).await;
    world.config_finalized(a, &["SEQ_B"], 3, 0).await;
    world.start("SEQ_B", reference("SEQ_B")).await;
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

/// A stale backlog is re-funded only as far as it will be posted: the
/// posted window and the next one. While the window cannot land, every
/// stale window re-funds those and leaves the rest of the queue on its old
/// notes. Re-funding every queued message instead reserved a fee note per
/// message per window (#3858); on testnet-legacy that emptied the wallet.
#[tokio::test(start_paused = true)]
async fn stale_backlog_reserves_fee_notes_only_for_its_window() {
    let mut world = World::with_funding(chains::linear(), 400, 3000);
    let depth = 10usize;
    let window = 4u64;
    let config = SequencerConfig {
        stale_refund_slots: window,
        ..world.config_with_depth(depth)
    };
    let a = world.start_with("SEQ_A", config, reference("SEQ_A")).await;
    let payloads: Vec<String> = (0..3 * depth).map(|i| format!("backlog-{i}")).collect();
    for payload in &payloads {
        world.publish(a, payload).await.expect("queued");
    }
    let queued = world.node().funding_reserved();
    assert_eq!(queued, payloads.len(), "each publish is funded once");

    // Blocks that take nothing: the posted window never lands, LIB moves
    // once it is a finality depth behind, and the stale windows pass with
    // the whole backlog waiting.
    let windows = 5usize;
    let blocks = usize::try_from(common::FINALITY_DEPTH + windows as u64 * window).expect("fits");
    for _ in 0..blocks {
        world.extend(1, Posted::None).await;
        world.harness.advance(World::STEP_DURATION).await;
    }
    let reserved = world.node().funding_reserved();
    let refunded = reserved - queued;
    assert!(
        refunded <= (windows + 1) * depth,
        "{refunded} notes re-funded for a backlog of {queued} with depth {depth} over {windows} windows:\n{}",
        world.diagnostics()
    );
    assert!(
        refunded > 0,
        "nothing was re-funded; the windows did not pass"
    );
    world.settle_chain(300).await;
    world.finish();
}

/// A dormant branch four deep is revived and wins, a twelve-deep branch
/// stays dormant, then a twenty-deep branch wins. Writers inscribe in every
/// block, fork blocks included, and re-land or rebuild what a reorg
/// orphans: a rebuilt inscription lands again as the same message under a
/// new tx hash, which the view reports as adopted without orphaning the
/// old hash. The contract is by message, so the view stays whole.
#[tokio::test(start_paused = true)]
async fn rebuilt_inscription_lands_as_the_same_message_after_a_deep_reorg() {
    let mut world = World::with_funding(chains::linear(), 400, 3000);
    let a = world.start("SEQ_A", passive()).await;
    world
        .config_finalized(a, &["SEQ_B", FOREIGN, FOREIGN_2], 3, 0)
        .await;
    world.stop(a);
    world.add_writer_with(FOREIGN, &[WriterOp::Inscribe]);
    world.add_writer_with(FOREIGN_2, &[WriterOp::Inscribe]);
    world.enable_foreign_traffic(8);
    world.harness.set_policy(a, reference("SEQ_A"));
    world.restart(a, None).await;
    world.start("SEQ_B", reference("SEQ_B")).await;

    let moves = [
        (
            Shape::Fork {
                depth: 4,
                length: 2,
            },
            Posted::All,
        ),
        (Shape::Revive { blocks: 3 }, Posted::All),
        (
            Shape::Fork {
                depth: 12,
                length: 11,
            },
            Posted::Half,
        ),
        (Shape::Extend { gap: 1 }, Posted::Reversed),
        (
            Shape::Fork {
                depth: 20,
                length: 21,
            },
            Posted::All,
        ),
        (Shape::Extend { gap: 1 }, Posted::All),
        (Shape::Extend { gap: 1 }, Posted::Reversed),
        (Shape::Extend { gap: 1 }, Posted::All),
    ];
    for (shape, posted) in moves {
        world.harness.advance(World::STEP_DURATION).await;
        world.apply_shape(shape, posted).await;
        world.harness.settle().await;
    }
    world.harness.assert_layers_through(Layer::Chain);
}
