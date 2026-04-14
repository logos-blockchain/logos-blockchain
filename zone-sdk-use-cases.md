# Zone SDK Sequencer — Use Cases & Design Rationale

## Overview

The zone-sdk sequencer manages inscription publishing on a channel. It handles:
- Connection management and reconnection
- Backfill on startup (catching up from checkpoint or genesis to current chain state)
- Automatic resubmission of pending inscriptions
- Detection of competing inscriptions and reorgs via `ChannelUpdate` events

The sequencer exposes two integration patterns:
1. **Spawn mode** — `sequencer.spawn()` runs the event loop in a background task; the caller uses `handle.publish()` and optionally `handle.subscribe()` to react to events.
2. **Direct polling** — the caller drives `sequencer.next_event()` manually inside their own `tokio::select!` loop, useful when the app already has an async runtime and wants to avoid spawning a separate task.

Both patterns use the same underlying state machine. The difference is who owns the event loop.

---

## Use Case 1: Single Sequencer (Simple)

A single sequencer owns the channel exclusively. No other sequencer publishes to the same channel.

```rust
let (sequencer, mut handle) = ZoneSequencer::init(channel_id, signing_key, node, checkpoint);
sequencer.spawn();
handle.wait_ready().await;

// Publish messages — fire and forget
handle.publish(b"Hello".to_vec()).await?;
handle.publish(b"World".to_vec()).await?;
```

### Why this works without resubmission logic

When a single sequencer owns the channel, **msg_ids never conflict**. Each inscription's
`msg_id` is derived from `(channel_id, parent_msg_id, payload, signer)`. Since there's only
one sequencer, it always builds on top of its own previous message — the parent chain is
linear and unambiguous.

The built-in resubmit timer handles transient failures (tx dropped from mempool, network
hiccup). It periodically resubmits pending inscriptions that haven't been included in a
block yet. No user intervention needed.

### When `ChannelUpdate` fires in single-sequencer mode

Even with one sequencer, `ChannelUpdate` can fire during L1 reorgs — a block containing
our inscription gets orphaned and replaced by a competing fork. In single-sequencer mode
the reorg doesn't introduce *competing* inscriptions (nobody else writes to the channel),
so the sequencer's pending inscriptions simply get resubmitted automatically. The user
doesn't need to handle `ChannelUpdate` events at all.

---

## Use Case 2: Competing Sequencers (Resubmission with Business Logic)

Multiple sequencers publish to the same channel. When two sequencers build inscriptions
with the same `parent_msg_id`, only one can win — the other gets orphaned.

```rust
let (sequencer, handle) = ZoneSequencer::init(channel_id, signing_key, node, checkpoint);
sequencer.spawn();

// Subscribe to events for conflict handling
let mut events = handle.subscribe();
let reorg_handle = handle.clone();

tokio::spawn(async move {
    loop {
        match events.recv().await {
            Ok(Event::ChannelUpdate { invalidated, adopted, .. }) => {
                // Determine which of our invalidated inscriptions
                // were NOT adopted on the new branch
                let adopted_payloads: HashSet<Vec<u8>> =
                    adopted.into_iter().map(|a| a.payload).collect();

                for inv in invalidated {
                    if !adopted_payloads.contains(&inv.payload) {
                        // Re-publish with the same payload.
                        // The sequencer assigns a new parent_msg_id
                        // and msg_id automatically.
                        let _ = reorg_handle.publish(inv.payload).await;
                    }
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
});
```

### Branch-aware state reconstruction

Each `ChannelUpdate` event gives the user a complete picture of the branch switch:
- **`invalidated`** — inscriptions that were on the old branch and are now orphaned
- **`adopted`** — inscriptions that appeared on the new canonical branch
- **`new_channel_tip`** — the current tip of the channel on the winning branch

This means the user can **reconstruct the state of the current canonical branch** at
every event and make informed resubmission decisions — not just "resubmit everything
blindly" but "given what's now on-chain, does this message still make sense?"

For example, a DeFi sequencer might decide:
- "My swap was orphaned, but the adopted branch contains a price update that makes
  my swap unprofitable — **don't resubmit**"
- "My deposit was orphaned but nothing on the new branch conflicts — **resubmit**"
- "My message was orphaned and an identical one from a competing sequencer was
  adopted — **skip** (already on-chain)"

The sequencer doesn't prescribe what to do with conflicts. It surfaces the full
branch state and lets the application's business logic decide. The simple
"resubmit if not adopted" handler shown above is one policy; real applications
can implement arbitrarily complex logic based on the `invalidated` and `adopted`
sets.

### Why resubmission must be payload-based, not msg_id-based

`msg_id` is derived from `(channel_id, parent_msg_id, payload, signer)`. When we
re-publish after a conflict, the `parent_msg_id` changes (because the channel tip moved
to whatever the competing sequencer published). This means the new inscription gets a
**different `msg_id`** than the original.

So we can't track "did my message make it" by msg_id — we track by **payload content**.
This is a deliberate design choice: the sequencer doesn't prescribe deduplication strategy;
it gives the user the payload and lets them decide.

### Deduplication strategies for real applications

The payload-matching approach above works for demos but real applications need a more
structured approach. Two patterns:

**Pattern A: Prefixed payloads (demo/simple)**

The TUI demo uses a random prefix per sequencer instance:
```
payload = format!("{prefix}:{user_message}")
```
This makes each sequencer's messages unique even if two users type the same text.
When checking `adopted_payloads.contains(&inv.payload)`, the prefix ensures exact
matching works. Simple, but the prefix is part of the on-chain data.

**Pattern B: Structured application state (production)**

A real sequencer would separate identity from content:
```rust
struct AppMessage {
    tx_id: Uuid,        // unique per logical message
    payload: Vec<u8>,   // actual content
}
```
The `tx_id` provides uniqueness. On reorg, the resubmission handler checks: "is there
an adopted inscription with the same `tx_id`?" If yes, skip (it was adopted via a
different sequencer or branch). If no, re-publish.

This keeps the deduplication logic in the application layer where it belongs — the
sequencer doesn't need to understand message semantics.

### Why the guard against double-publishing

In the resubmission handler above, we check `!adopted_payloads.contains(&inv.payload)`
before re-publishing. This prevents a subtle bug:

When a `ChannelUpdate` fires, `adopted` contains inscriptions that appeared on the
winning branch. If a competing sequencer published the **same payload** (e.g., both
sequencers received the same user request), the content is already on-chain via the
adopted inscription — re-publishing would create a duplicate.

This is the application's responsibility to decide: some use cases want duplicates
(append-only logs), others don't (exactly-once delivery). The guard shown here
implements at-most-once semantics per payload.

---

## Use Case 3: Direct Event Polling (Existing Async Environment)

When the application already has a `tokio::select!` loop (e.g., a service handling
multiple concerns), spawning the sequencer as a separate task adds unnecessary
complexity. Instead, drive the event loop directly:

```rust
let (mut sequencer, handle) = ZoneSequencer::init(channel_id, signing_key, node, checkpoint);

// Application's main loop
loop {
    tokio::select! {
        // Drive the sequencer — returns events as they occur
        Some(event) = sequencer.next_event() => {
            match event {
                Event::TxsFinalized { tx_hashes } => {
                    // Update application state
                    println!("Finalized: {tx_hashes:?}");
                }
                Event::ChannelUpdate { invalidated, adopted, new_channel_tip } => {
                    // Handle conflicts inline — no separate task needed
                    for inv in &invalidated {
                        // currently we spawn a task here, because publish is sync, but there is todo to fix this, which will be addressed in the PR
                        handle.publish(inv.payload.clone()).await.ok();
                    }
                }
                Event::FinalizedInscriptions { inscriptions } => {
                    // Backfill catch-up events during startup
                }
            }
        }

        // Application's own work
        msg = app_receiver.recv() => {
            if let Some(data) = msg {
                handle.publish(data).await.ok();
            }
        }
    }
}
```

### Why this pattern exists

The `spawn()` + `subscribe()` pattern creates a broadcast channel between the
sequencer's event loop and the subscriber. This works well for simple apps but has
trade-offs:

- **Extra task** — the sequencer runs in its own tokio task. For services that already
  manage many tasks, this is one more to track and shut down.
- **Channel backpressure** — the broadcast channel has a fixed buffer (256 events). If
  the subscriber falls behind, events are dropped.
- **Ordering** — events arrive asynchronously relative to the subscriber's own work.
  With `tokio::select!`, the application processes events inline, maintaining a single
  sequential flow.

The direct polling pattern avoids all three by letting the application own the event
loop. The sequencer is just another future in the `select!`, alongside the app's own
channels, timers, or I/O.

### When to use which

| Pattern | Best for |
|---|---|
| `spawn()` + `subscribe()` | Simple apps, CLIs, demos, when you don't have an existing async loop |
| `next_event()` + `select!` | Services, daemons, apps with existing event loops, when you need precise control over event ordering |

---

## Future: High-Level Resubmission Policies

The current API gives full control to the user: subscribe to events, decide what to
re-publish, implement your own deduplication. This is powerful but requires understanding
the event model.

For common use cases, we plan to offer built-in resubmission policies that handle
conflicts automatically:

```rust
// Example future API (not yet implemented):
let config = SequencerConfig {
    resubmission_policy: ResubmissionPolicy::RebaseOnConflict,
    ..Default::default()
};
let (sequencer, handle) = ZoneSequencer::init_with_config(
    channel_id, signing_key, node, None, config, checkpoint,
);
sequencer.spawn();

// Just publish — conflicts are handled automatically
handle.publish(data).await?;
```

Planned policies:

- **`RebaseOnConflict`** — automatically re-publish orphaned inscriptions on the new
  branch tip. Equivalent to the manual `ChannelUpdate` handler shown above. Suitable
  for append-only logs where every message must eventually land.

- **`DropOnConflict`** — discard orphaned inscriptions silently. Suitable for
  time-sensitive data where stale messages shouldn't be retried (e.g., price feeds).

- **`Custom(handler)`** — user provides a closure that receives `(invalidated, adopted)`
  and returns a list of payloads to re-publish. Full flexibility without managing the
  event subscription manually.

These policies would be opt-in. The raw event API remains available for users who
need full control over conflict resolution, deduplication, or integration with
external systems.
