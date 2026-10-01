# Bridging Assets Between the Bedrock and a Zone

A user guide for zone developers using the Zone SDK to move tokens between the Logos Blockchain and a zone over a channel.


## What "bridging" means here

A **channel** on Logos Blockchain is both the message log a zone publishes to *and* the bridge between Logos Blockchain-side balances and Zone-side balances. A channel holds its funds as **channel notes**: ledger notes registered to the channel, which only the channel's accredited keys can spend. Deposits add channel notes; withdrawals release them. The zone is free to define how those funds map to its own internal accounts — the SDK only surfaces the on-chain events.

Two directions:

- **Deposit** (Blockchain -> Zone). A user deposits notes into a channel via [`ChannelDeposit`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#80b261aa09df8353814a81efe0fbd8ed). Each deposited note is re-created as a channel note. The zone sequencer observes the finalized deposit and credits the user inside the zone according to `ChannelDeposit.metadata`.
- **Withdraw** (Zone -> Blockchain). The zone sequencer submits a `ChannelTransfer` that moves channel notes to the recipients, together with a [`ChannelWithdraw`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#cd7261aa09df83dd98b3017dafc37e87) that releases the recipients' notes, both signed by [`ChannelState.transfer_threshold`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca) accredited keys. The released notes become regular notes on the Blockchain, owned by the recipients' keys.

The Zone SDK is the client library for *zone-side* code: sequencers issuing withdraws and observing deposits, indexers replaying the message log.


## Creating a channel

Channels are not deployed by a separate transaction; Channels are created just-in-time on the first operation that references a previously unseen [`ChannelId`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca). A `ChannelId` is a 32-byte identifier chosen by the creator. Whoever signs that first operation becomes the sole accredited key, and the channel starts with `configuration_threshold = 1`, `transfer_threshold = 1`, and no channel notes.

From the Zone SDK, the steps are:

1. Pick a `ChannelId` and a sequencer `Ed25519Key`.
2. Initialize a `ZoneSequencer`, drive it (see the SDK overview for the drive-loop pattern and the `Event::Ready` readiness contract), and publish the first inscription (e.g., a zone genesis block) via `sequencer.handle().publish(..)`. On the Blockchain, Channels are created automatically, naming this sequencer as the sole accredited key.
3. (Optional) Reconfigure the channel with a [`ChannelConfig`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#f96261aa09df826a93d801db1e432a54) operation by calling `sequencer.handle().channel_config(..)`.

```rust
use lb_zone_sdk::{
    CommonHttpClient,
    adapter::NodeHttpClient,
    sequencer::{FundingConfig, ZoneSequencer},
};

let node = NodeHttpClient::new(
    CommonHttpClient::new(None),
    "http://localhost:8080".parse()?,
);

// Every publish-type call funds its transaction from the connected node's
// wallet before signing, so a funding config is mandatory. `funding_pk` is
// the public key of a wallet key that node controls — the same key the
// node's own configuration declares as its funding wallet. The node builds
// and proves the fee transfer; the secret key never leaves it.
let funding = FundingConfig {
    funding_pk,
    // Where an atomic withdraw's change goes; `None` sends it to `funding_pk`.
    change_pk: None,
    max_tx_fee: 1_000_000.into(),
    priority_fee_percent: FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT,
};
let mut sequencer = ZoneSequencer::init(channel_id, signing_key, node, funding, None);

// Inside the drive task, once `Event::Ready` has fired:
// publishing the first inscription creates the channel just-in-time.
let (result, checkpoint) = sequencer.handle().publish(genesis_zone_block).await?;
```

`priority_fee_percent` is a percentage reserve over the complete mandatory
fee, which consists of execution plus storage cost. Only the reserve left
after the transaction's final mandatory fee is charged becomes the effective
priority tip. The Zone SDK default is 12%: a practical reserve intended to
absorb normal fee movement, including approximately one storage-market epoch
increase under normal price levels. It is not a protocol guarantee at very low
prices or when execution fees also rise materially. Storage prices use integer
arithmetic, so low prices can make proportionally larger jumps (for example,
1 to 2); 12% is therefore a safety margin, not a guaranteed one-epoch bound.

To override other sequencer settings, build the config explicitly —
`SequencerConfig::new(funding)` fills in the defaults for everything else:

```rust
use lb_zone_sdk::sequencer::SequencerConfig;

let config = SequencerConfig {
    resubmit_interval: Duration::from_secs(10),
    ..SequencerConfig::new(funding)
};
let mut sequencer =
    ZoneSequencer::init_with_config(channel_id, signing_key, node, config, None);
```

`publish` returns synchronously after enqueueing the tx into the sequencer's pending set; the post hits the node the next time the drive loop polls `next_event`. The returned `PublishReceipt` carries everything you need to persist this publish into your outbox alongside the resulting checkpoint.

### The bridging-related fields in channel state

The bridging-relevant fields on [`ChannelState`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca) in the Mantle specification:

| Field                | Purpose                                                              |
| -------------------- | -------------------------------------------------------------------- |
| `transfer_threshold` | Minimum number of accredited-key signatures needed to authorize a channel transfer or withdraw. |
| `accredited_keys`    | The committee that may sign transfers and withdraws. |

The channel's funds are not a field of `ChannelState`: they are the channel notes registered to it. A transfer or withdraw spends specific notes by id, so it cannot be replayed once those notes are gone.


## Deposits: Bedrock -> zone

### What the Bedrock user submits

The Bedrock user submits a transaction with a [`ChannelDeposit`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#80b261aa09df8353814a81efe0fbd8ed) operation, naming the target `channel`, the `inputs` notes being consumed, and opaque `metadata` the zone will interpret (e.g., the recipient address).

The operation is proven with a `ZkSignature` over the consumed notes. On-chain execution spends the inputs and re-creates each one as a channel note with the same value and key.

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

### Observing before finality: pinning

`Event::BlocksProcessed` also carries `deposits`, the channel deposits observed in the block just processed, as `DepositInfo` with the deposit's `op_id`, `amount`, `metadata` and the channel notes it created. These are observations, not part of the channel view.

To credit a deposit before finality, **pin** it with `publish_pin_deposit(inscription, consumed_notes)`: an inscription bundled with a channel transfer that consumes the deposit's notes, so it can only land on a branch where the deposit exists. If the deposit is not on the current branch the call returns `Error::Network` and nothing is posted; pin it again on a later event. The pin surfaces as `ChannelUpdateTx::PinDeposit` in `adopted`, and in `orphaned` if it is shed, in which case pin again.

```rust
use lb_zone_sdk::sequencer::{ChannelUpdateTx, Error, Event};

if let Event::BlocksProcessed { channel_update, deposits, .. } = event {
    observed.extend(deposits.iter().cloned());
    for tx in channel_update.orphaned() {
        if let ChannelUpdateTx::PinDeposit(info) = tx {
            pinned.remove(&deposit_of(info));
        }
    }
    for deposit in &observed {
        if pinned.contains(&deposit.op_id) {
            continue;
        }
        let notes = deposit.notes.iter().map(|note| note.note_id).collect();
        match sequencer.handle().publish_pin_deposit(pin_payload_for(deposit), notes).await {
            Ok(_) => { pinned.insert(deposit.op_id); }
            Err(Error::Network(_)) => {} // not on this branch right now
            Err(e) => return Err(e),
        }
    }
}
```

`DepositLifecyclePolicy` in `tests/src/cucumber/steps/zone/operations/deposit_policy.rs` is the reference implementation.


## Withdrawals: Zone -> Blockchain

A withdraw is initiated *inside the zone* and lands on-chain as two signed operations in one transaction:

- a `ChannelTransfer` that spends channel notes covering the amount and creates the recipients' notes, plus any change, as new channel notes;
- a [`ChannelWithdraw`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#5de261aa09df8321b05401f2e8dea08b) that names the recipients' notes by id and releases them, so they stay on-chain as regular notes owned by the recipients' keys.

Each of the two carries a `ChannelMultiSigProof` with `ChannelState.transfer_threshold` signatures, at most one per accredited-key index.

### Single-sequencer zones

Currently, the Zone SDK supports the bundled withdrawal API only for single-sequencer zones (`ChannelState.transfer_threshold == 1`). You describe *what* to withdraw via a `WithdrawArg` — just the recipient `Outputs` — and which channel notes pay for it via `WithdrawInputs`: `WithdrawInputs::Auto` lets the SDK pick covering notes from the channel notes it tracks, while `WithdrawInputs::Explicit` names the exact notes, e.g. chosen from `sequencer.channel_wallet()`. The SDK fills in the `channel_id` and signs with this sequencer's accredited key, found in cached channel state.

```rust
use lb_core::mantle::{Note, ledger::Outputs};
use lb_zone_sdk::sequencer::{WithdrawArg, WithdrawInputs};

let withdraw = WithdrawArg {
    outputs: Outputs::new([Note::new(50, recipient_pk)]),
};

// Inside the drive task: submit the inscription bundled with the withdraw.
let (result, checkpoint) = sequencer
    .handle()
    .publish_atomic_withdraw(
        inscription_payload, // the zone block this withdraw goes with
        vec![withdraw],
        WithdrawInputs::Auto,
    )
    .await?;
```

Because the inscription, the transfer and the withdraw share one transaction, they adopt/orphan/finalize as a unit — the zone block recording the withdraw and the on-chain debit cannot drift apart.

#### Observing the result

`publish_atomic_withdraw` returns the `PublishResult` as soon as the bundle is queued. For a single-sig bundle, `PublishResult.tx` is a `PendingTx::AtomicWithdraw(AtomicWithdrawInfo)` carrying the inscription, the bundled withdraw ops and the recipient `outputs`. Persist it immediately as your outbox.

The same tx later resurfaces in `Event::BlocksProcessed.finalized` once the block containing it finalizes. Because it's a bundle, the inscription, the channel transfer and the withdraw all appear in the same `tx.ops` — match the `tx_hash` against your outbox and iterate the ops:

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
                FinalizedOp::ChannelTransfer(transfer) => {
                    // The channel notes moved to the recipients.
                    println!("Transferred {:?} in tx {:?}", transfer.op, transfer.tx_hash);
                }
                FinalizedOp::Withdraw(withdrawal) => {
                    // The recipients' notes released from the channel.
                    println!("Withdrawn {:?} in tx {:?}", withdrawal.op, withdrawal.tx_hash);
                }
                _ => {}
            }
        }
    }
}
```

### Multi-sequencer zones

When `transfer_threshold > 1`, no single sequencer can authorize a withdraw alone. The Zone SDK exposes the lower-level building blocks for threshold coordination, and the proposing sequencer builds the `ChannelTransferOp` and `ChannelWithdrawOp` itself (instead of a `WithdrawArg`), since `publish_atomic_withdraw` signs with the local key only.

- `handle.prepare_tx(ops, inscription)` — build the unsigned `Ops` for arbitrary `ops` (including `ChannelTransfer` and `ChannelWithdraw`), with the inscription appended last, and return it plus this sequencer's own signature.
- `handle.sign_tx(&tx)` — sign a transaction prepared elsewhere, e.g. one proposed by another committee member.
- `handle.submit_signed_tx(signed_tx, msg_id)` — submit once the committee has gathered `ChannelState.transfer_threshold` signatures.

The committee transport (how proposals and signatures are exchanged) is outside the Zone SDK's scope.

The proposing sequencer needs its own accredited-key index, surfaced on the sequencer's channel-view watch, and the channel notes that pay for the withdraw, e.g. chosen from `sequencer.channel_wallet()`:

```rust
use lb_core::mantle::{
    Op, SignedOps,
    ledger::{Inputs, Outputs},
    ops::{
        OpProof,
        channel::{channel_transfer::ChannelTransferOp, withdraw::ChannelWithdrawOp},
    },
    transactions::OpProofs,
};
use lb_core::proofs::channel_multi_sig_proof::{
    ChannelMultiSigProof, IndexedSignature, IndexedSignatures,
};

// 1. Read this sequencer's accredited-key index from the channel view,
//    kept fresh by the drive loop.
let view = sequencer.subscribe_channel_view().borrow().clone();
let own_key_index = view.own_key_index.ok_or("not an accredited key")?;

// 2. Build the unsigned tx and get this sequencer's own signature back. The
//    transfer spends channel notes covering the amount and creates the
//    recipients' notes, then any change, as channel notes; the withdraw
//    releases the recipients' notes, whose ids come from the transfer.
let transfer = ChannelTransferOp {
    channel_id,
    inputs: Inputs::try_new(channel_note_ids)?,
    outputs: Outputs::try_new(recipient_notes_then_change)?,
};
let recipient_note_ids: Vec<_> = transfer
    .utxos()
    .take(recipient_count)
    .map(|utxo| utxo.id())
    .collect();
let withdraw = ChannelWithdrawOp {
    channel_id,
    inputs: Inputs::try_new(recipient_note_ids)?,
};
let (tx, msg_id, own_sig) = sequencer.handle().prepare_tx(
    [Op::ChannelTransfer(transfer), Op::ChannelWithdraw(withdraw)].into(),
    inscription_payload,
)?;

// 3. Hand `tx` to the other accredited signers and collect their
//    `IndexedSignature`s. Transport is application-defined.
let signatures: Vec<IndexedSignature> = collect_signatures_from_committee(
    &tx,
    IndexedSignature::new(own_key_index, own_sig.clone()),
).await?;

// 4. Assemble the threshold proof and submit. `IndexedSignatures` orders the
//    signatures by key index and refuses a second signature for the same key.
//    The transfer and the withdraw sign the same tx hash, so they share the
//    proof; the inscription, last in `tx`, carries this sequencer's signature.
let signatures = IndexedSignatures::try_from_iter(signatures.into_iter().map(Into::into))?;
let channel_proof = ChannelMultiSigProof::new(signatures);
let signed_tx = SignedOps::from_parts(
    tx,
    OpProofs::from([
        OpProof::ChannelMultiSigProof(channel_proof.clone()),
        OpProof::ChannelMultiSigProof(channel_proof),
        OpProof::Ed25519Sig(own_sig),
    ]),
)?;
let (result, checkpoint) = sequencer
    .handle()
    .submit_signed_tx(signed_tx, msg_id)?;
```

#### Observing the result

`submit_signed_tx` returns the `PublishResult` synchronously. Unlike the single-sig flow, the SDK treats the caller-built tx as opaque, so `PublishResult.tx` is `PendingTx::Inscription(InscriptionInfo)` regardless of the underlying ops — the bundle structure is not reconstructed in the result. Persist it as your outbox using the returned `tx_hash` to identify the bundle.

The finalization pattern is the same as in the single-sig case: `Event::BlocksProcessed.finalized` carries the bundle's `FinalizedOp::ChannelTransfer`, `FinalizedOp::Withdraw` and `FinalizedOp::Inscription` in one `tx.ops`. Match by `tx_hash` and iterate the ops as shown above.

### Reorgs and republish

If a withdraw submitted via `publish_atomic_withdraw` has its parent inscription orphaned by a chain reorg, the SDK reports it via the `channel_update` field of `Event::BlocksProcessed`: the update is a `ChannelUpdate::Conflict` and the abandoned tx is in its `orphaned` list, reachable through `channel_update.orphaned()`. The original signed transaction is no longer valid. The consumer decides whether to republish — re-call `publish_atomic_withdraw` with the same inscription payload and a `WithdrawArg` rebuilt from the bundle's `outputs`; the SDK refills the inscription parent and picks covering channel notes again from current on-chain state.

```rust
use lb_zone_sdk::sequencer::{ChannelUpdateTx, Event, WithdrawArg, WithdrawInputs};

if let Event::BlocksProcessed { channel_update, .. } = event {
    for tx in channel_update.orphaned() {
        if let ChannelUpdateTx::AtomicWithdraw(info) = tx {
            let (result, checkpoint) = sequencer
                .handle()
                .publish_atomic_withdraw(
                    info.inscription.payload.clone(),
                    vec![WithdrawArg { outputs: info.outputs.clone() }],
                    WithdrawInputs::Auto,
                )
                .await?;
            // Persist `result` + `checkpoint` exactly as on the original publish.
        }
    }
}
```

For multi-sig bundles the orphan event carries the same `ChannelUpdateTx::AtomicWithdraw` data — whether the bundle was mined and reorged out, or never mined and shed once its parent slot was consumed by a competing entry — but the SDK cannot re-sign the tx: recovery means re-running the `prepare_tx` → collect-signatures → `submit_signed_tx` flow against the fresh parent, with channel notes that are still unspent.
