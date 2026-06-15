# Bridging Assets Between the Bedrock and a Zone

A user guide for zone developers using the Zone SDK to move tokens between the Logos Blockchain and a zone over a channel.


## What "bridging" means here

A **channel** on Logos Blockchain is both the message log a zone publishes to *and* the bridge between Logos Blockchain-side balances and Zone-side balances. Each channel carries an on-chain balance ([`ChannelState.balance`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca)); deposits credit it, withdrawals debit it. The zone is free to define how that balance maps to its own internal accounts — the SDK only surfaces the on-chain events.

Two directions:

- **Deposit** (Blockchain -> Zone). A user deposits notes into a channel via [`ChannelDeposit`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#80b261aa09df8353814a81efe0fbd8ed). The channel balance grows. The zone sequencer observes the finalized deposit and credits the user inside the zone according to `ChannelDeposit.metadata`.
- **Withdraw** (Zone -> Blockchain). The zone sequencer submits [`ChannelWithdraw`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#cd7261aa09df83dd98b3017dafc37e87), signed by [`ChannelState.withdraw_threshold`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca) accredited keys. The channel balance shrinks and fresh notes appear on the Blockchain for the recipients named in `ChannelWithdraw.outputs`.

The Zone SDK is the client library for *zone-side* code: sequencers issuing withdraws and observing deposits, indexers replaying the message log.


## Creating a channel

Channels are not deployed by a separate transaction; Channels are created just-in-time on the first operation that references a previously unseen [`ChannelId`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca). A `ChannelId` is a 32-byte identifier chosen by the creator. Whoever signs that first operation becomes the sole accredited key, and the channel starts with `configuration_threshold = 1`, `withdraw_threshold = 1`, and `balance = 0`.

From the Zone SDK, the steps are:

1. Pick a `ChannelId` and a sequencer `Ed25519Key`.
2. Initialize a `ZoneSequencer`, drive it (see the SDK overview for the drive-loop pattern and the `Event::Ready` readiness contract), and publish the first inscription (e.g., a zone genesis block) via `sequencer.handle().publish(..)`. On the Blockchain, Channels are created automatically, naming this sequencer as the sole accredited key.
3. (Optional) Reconfigure the channel with a [`ChannelConfig`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#f96261aa09df826a93d801db1e432a54) operation by calling `sequencer.handle().channel_config(..)`.

```rust
use lb_zone_sdk::{
    CommonHttpClient, adapter::NodeHttpClient, sequencer::ZoneSequencer,
};

let node = NodeHttpClient::new(
    CommonHttpClient::new(None),
    "http://localhost:8080".parse()?,
);
let mut sequencer = ZoneSequencer::init(channel_id, signing_key, node, None);

// Inside the drive task, once `Event::Ready` has fired:
// publishing the first inscription creates the channel just-in-time.
let (result, checkpoint) = sequencer.handle().publish(genesis_zone_block)?;
```

`publish` returns synchronously after enqueueing the tx into the sequencer's pending set; the post hits the node the next time the drive loop polls `next_event`. The returned `(PublishResult, SequencerCheckpoint)` carries everything you need to persist this publish into your outbox alongside the resulting checkpoint.

### The bridging-related fields in channel state

The bridging-relevant fields on [`ChannelState`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca) in the Mantle specification:

| Field                | Purpose                                                              |
| -------------------- | -------------------------------------------------------------------- |
| `balance`            | On-chain TokenValue held by the channel. Deposits add, withdraws subtract. |
| `withdrawal_nonce`   | Increments by 1 on every successful withdraw. Replay protection. |
| `withdraw_threshold` | Minimum number of accredited-key signatures needed to authorize a withdraw. |
| `accredited_keys`    | The committee that may sign withdraws. |


## Deposits: Bedrock -> zone

### What the Bedrock user submits

The Bedrock user submits a transaction with a [`ChannelDeposit`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#80b261aa09df8353814a81efe0fbd8ed) operation, naming the target `channel`, the `inputs` notes being consumed, and opaque `metadata` the zone will interpret (e.g., the recipient address).

The operation is proven with a `ZkSignature` over the consumed notes. On-chain execution spends the inputs and adds their value to `channel.balance`.

### What the zone sequencer sees

The Zone SDK surfaces every finalized deposit on your channel as a `FinalizedOp::Deposit(DepositInfo)` inside the `finalized` field of `Event::BlocksProcessed`.

`BlocksProcessed` fires per ingested block (live or backfill) and only carries finalized items at or below LIB, so deposits surfaced here cannot be re-orged off the chain.

```rust
use lb_zone_sdk::sequencer::{Event, FinalizedOp};

if let Event::BlocksProcessed { finalized, .. } = event {
    for tx in finalized {
        for op in tx.ops {
            if let FinalizedOp::Deposit(deposit) = op {
                println!(
                    "Deposit of {} with metadata {:?}",
                    deposit.amount, deposit.metadata,
                );
            }
        }
    }
}
```


## Withdrawals: Zone -> Blockchain

A withdraw is initiated *inside the zone* and lands on-chain as a signed [`ChannelWithdraw`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#5de261aa09df8321b05401f2e8dea08b) operation.

The `ChannelWithdraw` specifies the `channel`, the `outputs` (new notes to mint on-chain), and the `withdraw_nonce` that must match `ChannelState.withdrawal_nonce` for replay protection.

The `ChannelWithdrawOpProof` carries `ChannelState.withdraw_threshold` signatures from distinct `ChannelState.accredited_keys`.

### Single-sequencer zones

Currently, the Zone SDK supports the bundled withdrawal API only for single-sequencer zones (`ChannelState.withdraw_threshold == 1`). You describe *what* to withdraw via a `WithdrawArg` — just the recipient `Outputs`. The SDK fills in the `channel_id` and reads the current `withdraw_nonce` and this sequencer's accredited-key index from cached channel state.

```rust
use lb_core::mantle::{Note, ledger::Outputs};
use lb_zone_sdk::sequencer::WithdrawArg;

let withdraw = WithdrawArg {
    outputs: Outputs::new([Note::new(50, recipient_pk)]),
};

// Inside the drive task: submit the inscription bundled with the withdraw.
let (result, checkpoint) = sequencer.handle().publish_atomic_withdraw(
    inscription_payload,   // the zone block this withdraw goes with
    vec![withdraw],
)?;
```

Because the inscription and the withdraw share one transaction, they adopt/orphan/finalize as a unit — the zone block recording the withdraw and the on-chain debit cannot drift apart.

#### Observing the result

`publish_atomic_withdraw` returns the `PublishResult` synchronously. For a single-sig bundle, `PublishResult.tx` is a `PendingTx::AtomicWithdraw(AtomicWithdrawInfo)` carrying the inscription and the bundled withdraw ops. Persist it immediately as your outbox.

The same tx later resurfaces in `Event::BlocksProcessed.finalized` once the block containing it finalizes. Because it's a bundle, **both** the inscription and the withdraw appear in the same `tx.ops` — match the `tx_hash` against your outbox and iterate both ops:

```rust
use lb_zone_sdk::sequencer::{Event, FinalizedOp};

if let Event::BlocksProcessed { finalized, .. } = event {
    for tx in finalized {
        for op in tx.ops {
            match op {
                FinalizedOp::Inscription(info) => {
                    // The zone block carried with the withdraw.
                    println!("Inscribed {:?} in tx {:?}", info.this_msg, info.tx_hash);
                }
                FinalizedOp::Withdraw(withdrawal) => {
                    // The on-chain debit.
                    println!("Withdrawn {:?} in tx {:?}", withdrawal.op, withdrawal.tx_hash);
                }
                FinalizedOp::Deposit(_) => {}
            }
        }
    }
}
```

### Multi-sequencer zones

When `withdraw_threshold > 1`, no single sequencer can authorize a withdraw alone. The Zone SDK exposes the lower-level building blocks for threshold coordination, and the proposing sequencer builds the `ChannelWithdrawOp` itself (instead of `WithdrawArg`) because it needs to commit to a specific `withdraw_nonce` before sharing the unsigned tx with the rest of the committee.

- `handle.prepare_tx(ops, inscription)` — build the unsigned `MantleTx` for arbitrary `ops` (including `ChannelWithdraw`) and return it plus this sequencer's own signature.
- `handle.sign_tx(&tx)` — sign a transaction prepared elsewhere, e.g. one proposed by another committee member.
- `handle.submit_signed_tx(signed_tx, msg_id)` — submit once the committee has gathered `ChannelState.withdraw_threshold` signatures.

The committee transport (how proposals and signatures are exchanged) is outside the Zone SDK's scope.

The proposing sequencer needs the current `withdraw_nonce` and its own accredited-key index. Both are surfaced on the sequencer's channel-view watch:

```rust
use lb_core::mantle::{
    Op, SignedMantleTx,
    ops::{OpProof, channel::withdraw::ChannelWithdrawOp},
};
use lb_core::proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature};

// 1. Read current nonce + this sequencer's accredited-key index from
//    the channel view. Both are kept fresh by the drive loop.
let view = sequencer.subscribe_channel_view().borrow().clone();
let withdraw_nonce = view
    .channel
    .as_ref()
    .ok_or("channel state not yet available")?
    .withdrawal_nonce;
let own_key_index = view.own_key_index.ok_or("not an accredited key")?;

// 2. Build the unsigned tx and get this sequencer's own signature back.
let withdraw = ChannelWithdrawOp {
    channel_id,
    outputs,
    withdraw_nonce,
};
let (tx, msg_id, own_sig) = sequencer.handle().prepare_tx(
    [Op::ChannelWithdraw(withdraw)].into(),
    inscription_payload,
)?;

// 3. Hand `tx` to the other accredited signers and collect their
//    `IndexedSignature`s. Transport is application-defined.
let signatures: Vec<IndexedSignature> = collect_signatures_from_committee(
    &tx,
    IndexedSignature::new(own_key_index, own_sig.clone()),
).await?;

// 4. Assemble the threshold proof and submit.
let withdraw_proof = ChannelMultiSigProof::new(signatures)?;
let signed_tx = SignedMantleTx::new(
    tx,
    vec![
        OpProof::ChannelMultiSigProof(withdraw_proof),
        OpProof::Ed25519Sig(own_sig),
    ],
)?;
let (result, checkpoint) = sequencer
    .handle()
    .submit_signed_tx(signed_tx, msg_id)?;
```

#### Observing the result

`submit_signed_tx` returns the `PublishResult` synchronously. Unlike the single-sig flow, the SDK treats the caller-built tx as opaque, so `PublishResult.tx` is `PendingTx::Inscription(InscriptionInfo)` regardless of the underlying ops — the bundle structure is not reconstructed in the result. Persist it as your outbox using the returned `tx_hash` to identify the bundle.

The finalization pattern is the same as in the single-sig case: `Event::BlocksProcessed.finalized` carries `FinalizedOp::Inscription` and `FinalizedOp::Withdraw` for the bundle in one `tx.ops`. Match by `tx_hash` and iterate both ops as shown above.

### Reorgs and republish

If a withdraw submitted via `publish_atomic_withdraw` has its parent inscription orphaned by a chain reorg, the SDK reports it via the `channel_update` field of `Event::BlocksProcessed`, with the abandoned tx in `channel_update.orphaned`. The original signed transaction is no longer valid. The consumer decides whether to republish — re-call `publish_atomic_withdraw` with the same inscription payload and `WithdrawArg`s reconstructed from the bundle; the SDK refills the inscription parent and the `withdraw_nonce` from current on-chain state.

```rust
use lb_zone_sdk::sequencer::{Event, OrphanedTx, WithdrawArg};

if let Event::BlocksProcessed { channel_update, .. } = event {
    for tx in channel_update.orphaned {
        if let OrphanedTx::AtomicWithdraw(info) = tx {
            let withdraws = info
                .withdraws
                .into_iter()
                .map(|w| WithdrawArg { outputs: w.op.outputs })
                .collect();
            let (result, checkpoint) = sequencer.handle().publish_atomic_withdraw(
                info.inscription.payload,
                withdraws,
            )?;
            // Persist `result` + `checkpoint` exactly as on the original publish.
        }
    }
}
```

The reorg-aware recovery path for multi-sig is **not** supported by the SDK at the moment and is planned for a future release.
