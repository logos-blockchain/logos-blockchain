# Decentralized Sequencing for Channels

A user guide for zone developers using the Zone SDK to operate a channel with multiple sequencers, with round-robin style publish slot windows.


## What "decentralized sequencing" means here

A **channel** on Logos Blockchain can be operated by a single sequencer (centralized) or by a committee of accredited sequencers that await their slot windows to publish inscriptions (decentralized). The on-chain rotation is enforced by Logos Blockchain itself: at any block slot, exactly one accredited key is *authorized* to write, derived deterministically from the channel's configuration plus the current slot. Other sequencers' inscriptions in that slot are rejected.

Two committees are tracked per channel:

- **Inscription committee** — the `accredited_keys` list, with rotation governed by `posting_timeframe` and `posting_timeout`. One key is authorized per slot via a round-robin algorithm.
- **Configuration committee** — the same `accredited_keys`, but configuration changes (rotating in/out, threshold changes) require `configuration_threshold` signatures across the committee.

Sequencers in a decentralized zone:

- Subscribe to their own turn-to-write status to know when they can publish.
- Publish freely at any time — the SDK queues inscriptions locally when it's not the sequencer's turn and posts them automatically when the turn arrives.
- Coordinate off-chain on what messages should be inscribed (the SDK does not provide cross-sequencer coordination; that's the application's concern).
- Co-sign configuration changes off-chain when `configuration_threshold > 1`.

The Zone SDK exposes the per-sequencer view of this model. It does not orchestrate the committee.


## Channel state fields for rotation

The rotation-relevant fields on [`ChannelState`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca):

| Field                       | Purpose                                                              |
| --------------------------- | -------------------------------------------------------------------- |
| `accredited_keys`           | Ordered list of `Ed25519PublicKey`s authorized to write or co-sign config changes. The round-robin index references positions in this list. |
| `posting_timeframe`         | Length (in slots) of each sequencer's turn before rotation moves to the next index. |
| `posting_timeout`           | Per-turn timeout used by the on-chain validator to advance rotation past an inactive sequencer. (See the Mantle spec for the full algorithm.) |
| `configuration_threshold`   | Minimum number of accredited-key signatures needed to update channel config. |
| `tip_message`               | The current channel-tip `MsgId`. New inscriptions must parent here. |

These fields are read by the on-chain `round_robin(slot)` function to compute who is authorized at any given slot.


## Round-robin rotation

The Zone SDK currently supports **round-robin** as the rotation scheme. Other scheduling mechanisms (e.g., First-Write-Wins) may be added later; for now, every decentralized channel rotates round-robin.

Round-robin assigns turns in order through the `accredited_keys` list. In a _normal_ 
setup, each sequencer holds its turn for `posting_timeframe` slots before rotation 
advances to the next index, wrapping back to 0 after the last sequencer. 

A busy channel will have cadence (turn-to-write rotation) every `posting_timeframe` 
slots. With silence, the current turn-to-write can be extended up to `posting_timeout` slots, 
after which the cadence will change to `posting_timeout` slots, until some sequencer 
publish in turn, then cadence will be reset to `posting_timeframe` slots again.

We also have some caveats: 
**Non-zero cases**
- `posting_timeout > posting_timeframe`: as discussed above - cadence is governed 
   by timeframe only until first timeout threshold is reached; after that, timeout 
   branch has priority, so effective cadence is governed by timeout windows (until a 
   write resets baseline).
- `0 < posting_timeout < posting_timeframe`: cadence is governed by timeout slots 
  (timeout branch dominates as soon as it can fire).
**Zero cases**
- `posting_timeframe > 0`, `posting_timeout = 0`: cadence is governed by timeframe 
  slots.
- `posting_timeframe = 0`, `posting_timeout > 0`: cadence is governed by timeout 
  slots.
- `posting_timeframe = 0`, `posting_timeout = 0`: no rotation at all — only the 
  first accredited key (`accredited_keys[0]`) is authorized to publish inscriptions, 
  with no slot window constraints. The configuration must be updated to change this.

**Note:** The Mantle spec is the source of truth for the exact algorithm.

### Sizing `posting_timeframe` and `posting_timeout`

`posting_timeframe` must be **at least the average slots-per-block** of the host chain. A turn shorter than one block means multiple sequencers can become authorized within the same block (since a block-producing leader picks up whatever the mempool has at proposal time), defeating the purpose of rotation. As a rule of thumb pick a multiple — e.g. with a ~20 slots/block average, use `posting_timeframe = 60` to give each sequencer roughly three blocks of "owned" leadership per turn.

`posting_timeout` should be ≥ `posting_timeframe`. Smaller values dominate the rotation cadence the moment the authorized sequencer goes silent (see the zero/non-zero cases above).

### Worked example

A channel with three accredited keys, `posting_timeframe = 60`, and `posting_timeout = 180` (so a silent sequencer is skipped after three full turn windows of inactivity):

```
slots 0–59     → index 0
slots 60–119   → index 1
slots 120–179  → index 2
slots 180–239  → index 0
...
```

If index 1's turn elapses without a publish, the on-chain validator advances past it once the `posting_timeout` window is reached.

The SDK exposes the current rotation state to your sequencer in two forms:

- The full `SequencerChannelView` (`subscribe_channel_view`) — a `tokio::sync::watch` receiver carrying `authorized_key_index`, `own_key_index`, `our_turn_to_write`, the current slot, and the turn window's `turn_to_write_slots`.
- A focused `TurnNotification` (`subscribe_turn_to_write`) plus `Event::TurnNotification` — emitted only when the turn boundary actually changes.

`our_turn_to_write` is the boolean a sequencer reads to decide whether it's currently authorized.


## The publish contract for decentralized sequencers

You do not need to gate `publish` calls on `our_turn_to_write` yourself. The SDK does it for you:

- **If it is our turn:** `publish` enqueues the tx into the sequencer's pending set *and* immediately queues the network post.
- **If it is not our turn:** `publish` still enqueues the tx into the pending set, but skips the network post. The tx stays in pending with `posted: false`.
- **When our turn arrives:** The SDK detects the `false → true` transition on `our_turn_to_write` and automatically drains pending publishes, posting them up to a configurable depth limit (`SequencerConfig.max_pending_publish_depth`).
- **Self-heal:** A periodic `resubmit_interval` (default 30 s) also drains pending publishes if our turn is current — covers cases where a post failed at the network level.

This means a sequencer can call `publish` from any task at any moment; the SDK will hold the tx until the rotation reaches it. The returned `PublishResult` reflects the enqueued state, not a network acknowledgement (same contract as in the bridging flow).

```rust
// From the drive task, once `Event::Ready` has fired.
let (result, checkpoint) = sequencer.handle().publish(zone_block)?;
// `result.tx` is a `PendingTx::Inscription(...)`.
// `checkpoint` is up to date with the new pending entry.
// The post may not have hit the node yet if it is not our turn.
```


## Reading turn state from the application

Two subscription channels surface turn state; use whichever fits your UI.

### Edge-triggered: `TurnNotification`

Fires once per change boundary. Best for "wake the UI when something happens" patterns.

```rust
use lb_zone_sdk::sequencer::Event;

if let Event::TurnNotification { notification } = event {
    if notification.our_turn_to_write {
        println!(
            "Our turn until slot {:?}",
            notification.ends_at_slot,
        );
    } else {
        println!(
            "Waiting for our turn; current authorized window ends at {:?}",
            notification.ends_at_slot,
        );
    }
}
```

### Snapshot: `SequencerChannelView`

The full view is updated on every block; subscribers can `borrow()` the current value at any time without waiting. Best for rendering a status panel.

```rust
let view_rx = sequencer.subscribe_channel_view();
let view = view_rx.borrow().clone();
if view.our_turn_to_write {
    // Authorized right now.
} else if let (Some(own), Some(auth)) = (view.own_key_index, view.authorized_key_index) {
    println!(
        "Sequencer {} is publishing; we are sequencer {}.",
        auth, own,
    );
}
```

The view carries `pending_publish_txs` and `queued_messages` so the UI can show how many inscriptions are waiting for our next turn.


## Adding and removing sequencers

The committee is changed by submitting a `ChannelConfig` op — same primitive used during channel creation. Currently only the single signer flow is supported through the SDK, but we outline both flows here.

### Single-signer: `configuration_threshold == 1`

When `configuration_threshold == 1`, a single valid signature from any one of the channel's accredited keys authorizes a configuration change. Because `ChannelConfigOp` overwrites the entire key list, re-include any keys that should remain accredited after the update.

```rust
use lb_core::mantle::channel::{Keys, SlotTimeframe, SlotTimeout};

let new_keys = Keys::from(vec![
    admin_pk,
    new_sequencer_b_pk,
    new_sequencer_c_pk,
]);

let (result, checkpoint, signed_tx) = sequencer.handle().channel_config(
    new_keys,
    SlotTimeframe::from(60),   // ~3 blocks per turn at 20 slots/block
    SlotTimeout::from(180),    // skip after 3 turn windows of inactivity
    1,                          // configuration_threshold (still single-admin)
    1,                          // withdraw_threshold
)?;
```

`channel_config` builds, signs (with the local key only), and submits the tx atomically. On finalization, the SDK's `refresh_channel_state` picks up the new config and the next `BlocksProcessed` event recomputes `our_turn_to_write` for every running sequencer.

### Multi-admin: `configuration_threshold > 1` (not yet wired)

When the channel has been moved to a threshold larger than 1, `handle.channel_config` is no longer sufficient — it only signs with the local key. The SDK exposes the lower-level primitives but **does not orchestrate signature collection across sequencers**; the committee transport (how the unsigned tx and the signatures are exchanged) is the application's responsibility.

The flow mirrors the multi-sig withdraw flow described in [`BRIDGING.md`](BRIDGING.md#multi-sequencer-zones): one sequencer proposes by calling `prepare_tx` with a `ChannelConfigOp` payload, distributes the unsigned tx to the rest of the committee, each co-signer calls `sign_tx`, the proposer assembles a `ChannelMultiSigProof` from the collected `IndexedSignature`s, and submits via `submit_signed_tx`.

```rust
use lb_core::mantle::{
    Op, SignedMantleTx,
    ops::{OpProof, channel::config::ChannelConfigOp},
};
use lb_core::proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature};

// 1. Read this sequencer's index from the channel view.
let view = sequencer.subscribe_channel_view().borrow().clone();
let own_key_index = view.own_key_index.ok_or("not an accredited key")?;

// 2. Build the unsigned config tx and get this sequencer's own signature.
let config = ChannelConfigOp {
    channel,
    keys: new_keys,
    posting_timeframe,
    posting_timeout,
    configuration_threshold,
    withdraw_threshold,
};
let (tx, msg_id, own_sig) = sequencer.handle().prepare_tx(
    [Op::ChannelConfig(config)].into(),
    inscription_payload,
)?;

// 3. Distribute `tx` to the rest of the committee, collect their
//    `IndexedSignature`s. Transport is application-defined.
let signatures: Vec<IndexedSignature> = collect_signatures_from_committee(
    &tx,
    IndexedSignature::new(own_key_index, own_sig.clone()),
).await?;

// 4. Assemble the threshold proof and submit.
let config_proof = ChannelMultiSigProof::new(signatures)?;
let signed_tx = SignedMantleTx::new(
    tx,
    vec![
        OpProof::ChannelMultiSigProof(config_proof),
        OpProof::Ed25519Sig(own_sig),
    ],
)?;
let (result, checkpoint) = sequencer
    .handle()
    .submit_signed_tx(signed_tx, msg_id)?;
```

The multi-admin flow is not currently exercised by integration tests (see `tests/cucumber_tests/features/zone.feature` in the repo); treat it as the reference path until SDK support lands.


## Competing writes and republish

In a decentralized channel, two sequencers can race on the same parent slot — for instance, when rotation is mid-transition or when sequencers are temporarily disconnected and resync at different rates. The on-chain rule is *first valid inscription wins*; the loser's tx becomes invalid because its parent slot is now claimed.

The SDK detects this via the `channel_update` field of `Event::BlocksProcessed`: the losing tx surfaces in `channel_update.orphaned` as an `OrphanedTx::Inscription(InscriptionInfo)`. The consumer decides whether to republish — re-call `publish` with the same payload and the SDK fills in the new (current) parent.

```rust
use lb_zone_sdk::sequencer::{Event, OrphanedTx};

if let Event::BlocksProcessed { channel_update, .. } = event {
    for entry in channel_update.orphaned {
        if let OrphanedTx::Inscription(info) = entry {
            let (result, checkpoint) = sequencer.handle().publish(info.payload)?;
            // Persist `result` + `checkpoint` exactly as on the original publish.
        }
    }
}
```

The integration tests provide reference policies that run this re-publish loop automatically, in `tests/src/cucumber/steps/manual_zone/support.rs`: `OrphanRepublishPolicy` (simple republish), `RepublishLineagePolicy` (for repeating payloads, tracks msg-id lineage so each intent lands once), `SortedConflictPolicy` (republish only when it preserves the channel's sorted order, otherwise discard), and `BalanceAwarePolicy` (republish only when the account balance still allows it). Wiring one into the drive loop is the recommended pattern for any sequencer running in a competing-write environment.

If the orphan policy is too aggressive — e.g., the orphan was caused by genuine application-level conflict, not a race — the consumer can choose to drop the payload, deduplicate against a higher-level transaction stream, or apply any other custom rule. The SDK only surfaces the event; the resolution policy is yours.


## Current limitations

- **`channel_config` is single-sig only.** `SequencerHandle::channel_config` signs with the local sequencer's key only, so it works for `configuration_threshold == 1` channels. For threshold > 1, use the `prepare_tx` + `submit_signed_tx` flow above. An SDK helper to drive the multi-admin flow end-to-end is not yet provided.

- **`publish_atomic_withdraw` requires `withdraw_threshold == 1`.** Multi-sig withdraws follow the same pattern as multi-sig config: `prepare_tx` → distribute → collect signatures → `submit_signed_tx`. See [`BRIDGING.md`](BRIDGING.md#multi-sequencer-zones) for the worked example.

- **No SDK-level coordination layer.** Off-chain coordination (who proposes config changes, how unsigned txs and signatures are transported between sequencers, how orphan-republish is gated across competing writers) is the application's responsibility. The SDK surfaces per-sequencer state and accepts independent calls; coordination is built on top.

- **Reorg-aware recovery for multi-sig.** The SDK tracks single-sig atomic withdraws as bundles and surfaces them as `OrphanedTx::AtomicWithdraw` on reorg. Multi-sig txs submitted via `submit_signed_tx` are tracked opaquely — no `OrphanedTx::AtomicWithdraw` event is emitted for them. This is documented in [`BRIDGING.md`](BRIDGING.md#reorgs-and-republish) and applies equally to multi-sig config changes.
