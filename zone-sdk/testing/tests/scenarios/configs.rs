//! Channel configs: re-keying, orphaning by config, posting timeouts.

use super::*;

/// S11: a config landing orphans pending inscriptions which re-adopt in order.
pub async fn config_orphans_pending(chain: Chain) {
    let mut world = World::new(chain);
    let config = world.config_with_depth(usize::MAX);
    let a = world.start_with("SEQ_A", config.clone(), passive()).await;
    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    let b = world.start_with("SEQ_B", config, reference("SEQ_B")).await;
    assert!(
        world
            .step_until(40, |w| not_our_turn_with_pending(w, b, 0))
            .await
    );
    for payload in ["cfg-orphan-b1", "cfg-orphan-b2", "cfg-orphan-b3"] {
        world.publish(b, payload).await.expect("queued");
    }
    assert!(
        world
            .step_until(40, |w| not_our_turn_with_pending(w, b, 3))
            .await
    );
    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    world.settle_chain(200).await;

    let expected = ["cfg-orphan-b1", "cfg-orphan-b2", "cfg-orphan-b3"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        filtered_first_occurrence(&payloads, &expected),
        strs(&expected)
    );
    world.finish();
}
scenario!(s11_config_orphans_pending, config_orphans_pending);

/// S23: a non-leading sequencer re-keys the channel; in-flight inscriptions
/// recover.
pub async fn rekey_by_non_leader(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    world.publish_included(a, "inscription before re-key").await;
    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    let b = world.start("SEQ_B", reference("SEQ_B")).await;
    world
        .publish(a, "in flight during re-key")
        .await
        .expect("publish");
    let rekey = world.submit_config(b, &["SEQ_A"], 2, 0).await;
    world
        .publish(b, "post re-key from B")
        .await
        .expect("publish");
    world
        .publish(a, "post re-key from A")
        .await
        .expect("publish");
    world.include(rekey).await;
    world.settle_chain(200).await;

    let expected = [
        "inscription before re-key",
        "in flight during re-key",
        "post re-key from B",
        "post re-key from A",
    ];
    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &expected);
    world.finish();
}
scenario!(s23_rekey_by_non_leader, rekey_by_non_leader);

/// S25: with no timeframe, a silent tip sequencer loses the turn after the
/// posting timeout and the waiting sequencer's queued message lands.
pub async fn timeout_rotates_silent_turn(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 0, 3).await;
    let b = world.start("SEQ_B", passive()).await;

    let a1 = world.publish_included(a, "a1").await;
    let b1 = world
        .publish(b, "b1")
        .await
        .expect("queued")
        .inscription_id();
    world.include(b1).await;
    assert!(
        included_slot(&world, &b1) >= included_slot(&world, &a1) + 3,
        "b1 landed before SEQ_A's turn timed out"
    );

    let a2 = world
        .publish(a, "a2")
        .await
        .expect("queued")
        .inscription_id();
    world.include(a2).await;
    assert!(
        included_slot(&world, &a2) >= included_slot(&world, &b1) + 3,
        "a2 landed before SEQ_B's turn timed out"
    );
    world.settle_chain(200).await;
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        filtered_first_occurrence(&payloads, &["a1", "b1", "a2"]),
        strs(&["a1", "b1", "a2"])
    );
    world.finish();
}
scenario!(s25_timeout_rotates_silent_turn, timeout_rotates_silent_turn);

/// S26: once the tip is silent past the timeout, the timeout grid overrides
/// the timeframe rotation; a sequencer two places down waits for its timeout
/// window and still publishes.
pub async fn timeout_overrides_timeframe(chain: Chain) {
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B", "SEQ_C"], 4, 5).await;
    let c = world.start("SEQ_C", passive()).await;

    let a1 = world.publish_included(a, "a1").await;
    let c1 = world
        .publish(c, "c1")
        .await
        .expect("queued")
        .inscription_id();
    world.include_within(c1, 60).await;
    // SEQ_B never posts, so after the timeout the turn goes B then C on the
    // timeout grid from a1's slot.
    let (a1_slot, c1_slot) = (included_slot(&world, &a1), included_slot(&world, &c1));
    assert!(
        c1_slot >= a1_slot + 2 * 5,
        "c1 landed at slot {c1_slot} before SEQ_C's timeout window (a1 at {a1_slot})"
    );
    world.settle_chain(200).await;
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        filtered_first_occurrence(&payloads, &["a1", "c1"]),
        strs(&["a1", "c1"])
    );
    world.finish();
}
scenario!(s26_timeout_overrides_timeframe, timeout_overrides_timeframe);
