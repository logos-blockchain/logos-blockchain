# Bridging Assets Between the Bedrock and a Zone

A user guide for zone developers using the Zone SDK to move tokens between the Logos blockchain (Bedrock) and a zone over a channel.


## What "bridging" means here

A **channel** on Bedrock is both the message log a zone publishes to *and* the bridge between Bedrock-side balances and zone-side balances. Each channel carries an on-chain balance ([`ChannelState.balance`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca)); deposits credit it, withdrawals debit it. The zone is free to define how that balance maps to its own internal accounts — the SDK only surfaces the on-chain events.

Two directions:

- **Deposit** (Bedrock -> zone). A user spends notes on Bedrock into a channel via [`ChannelDeposit`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#80b261aa09df8353814a81efe0fbd8ed). The channel balance grows. The zone sequencer observes the finalized deposit and credits the user inside the zone according to `ChannelDeposit.metadata`.
- **Withdraw** (zone -> Bedrock). The zone sequencer submits [`ChannelWithdraw`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#cd7261aa09df83dd98b3017dafc37e87), signed by [`ChannelState.withdraw_threshold`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca) accredited keys. The channel balance shrinks and fresh notes appear on Bedrock for the recipients named in `ChannelWithdraw.outputs`.

The Zone SDK is the client library for *zone-side* code: sequencers issuing withdraws and observing deposits, indexers replaying the message log.


## Creating a channel

Channels are not deployed by a separate transaction; Bedrock creates them just-in-time on the first operation that references a previously unseen [`ChannelId`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#22b261aa09df8289a3f281de4aa8fdca). A `ChannelId` is a 32-byte identifier chosen by the creator. Whoever signs that first operation becomes the sole accredited key, and the channel starts with `configuration_threshold = 1`, `withdraw_threshold = 1`, and `balance = 0`.

From the Zone SDK, the steps are:

1. Pick a `ChannelId` and a sequencer `Ed25519Key`.
2. Initialize a `ZoneSequencer` with both, then publish the first inscription (e.g., a zone genesis block) via `handle.publish_message(..)`. Bedrock creates the channel automatically, naming this sequencer as the sole accredited key.
3. (Optional) Reconfigure the channel with a [`ChannelConfig`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#f96261aa09df826a93d801db1e432a54) operation by calling `handle.channel_config(..)`.

```rust
use lb_zone_sdk::{
    CommonHttpClient, adapter::NodeHttpClient, sequencer::ZoneSequencer,
};

let node = NodeHttpClient::new(
    CommonHttpClient::new(None),
    "http://localhost:8080".parse()?,
);
let (sequencer, handle) =
    ZoneSequencer::init(channel_id, signing_key, node, None);

// Publishing the first inscription creates the channel just-in-time.
handle.publish_message(genesis_zone_block).await?;
```

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

The Zone SDK surfaces every finalized deposit on your channel as a `FinalizedOp::Deposit(DepositInfo)` inside `Event::TxsFinalized`.

This event only fires for transactions in finalized (irreversible) blocks. Therefore, deposits surfaced here cannot be re-orged off the chain.

```rust
use futures::StreamExt as _;
use lb_zone_sdk::sequencer::{Event, FinalizedOp, ZoneSequencer};

let mut events = sequencer.events();
while let Some(event) = events.next().await {
    if let Event::TxsFinalized { items } = event {
        for tx in items {
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
}
```


## Withdrawals: zone -> Bedrock

A withdraw is initiated *inside the zone* and lands on Bedrock as a signed [`ChannelWithdraw`](https://app.notion.com/p/nomos-tech/1-5-0-Mantle-33d261aa09df8051b0d0cd4d5ddade85?source=copy_link#5de261aa09df8321b05401f2e8dea08b) operation.

The `ChannelWithdraw` specifies the `channel`, the `outputs` (new notes to mint on Bedrock), and the `withdraw_nonce` that must match `ChannelState.withdrawal_nonce` for replay protection.

The `ChannelWithdrawOpProof` carries `ChannelState.withdraw_threshold` signatures from distinct `ChannelState.accredited_keys`.

### Single-sequencer zones

Currently, the Zone SDK supports the withdrawal API only for single-sequencer zones (`ChannelState.withdraw_threshold == 1`).
```rust
use lb_zone_sdk::sequencer::WithdrawArg;
use lb_core::mantle::ledger::Outputs;

// Build the withdraw arguments with the intended outputs.
let withdraw = WithdrawArg {
    outputs: Outputs::from(vec![
        // Note { pk: recipient, value: 50, .. }
    ]),
};

// Submit a transaction with the withdraw operation and
// an accompanying inscription (e.g., zone block).
handle
    .publish_atomic_withdraw(
        inscription_payload,   // the zone block this withdraw goes with
        vec![withdraw],
    )
    .await?;
```
Because the inscription and the withdraw share one transaction, they adopt/orphan/finalize as a unit — the zone block recording the withdraw and the on-chain debit cannot drift apart.

### Multi-sequencer zones

When `withdraw_threshold > 1`, no single sequencer can authorize a withdraw alone. The Zone SDK exposes the lower-level building blocks for threshold coordination:

- `handle.prepare_tx(ops, inscription)` — build the unsigned `MantleTx` for arbitrary `ops` (including `ChannelWithdraw`) and return it plus this sequencer's own signature.
- `handle.sign_tx(&tx)` — sign a transaction prepared elsewhere, e.g. one proposed by another committee member.
- `handle.submit_signed_tx(signed_tx, msg_id)` — submit once the committee has gathered `ChannelState.withdraw_threshold` signatures.

The committee transport (how proposals and signatures are exchanged) is outside the Zone SDK's scope.

```rust
use lb_core::mantle::{Op, SignedMantleTx, ops::channel::withdraw::ChannelWithdrawOp};
use lb_core::proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature};
use lb_zone_sdk::sequencer::OpProof;

// 1. The proposing sequencer builds the unsigned transaction and returns
//    it plus this sequencer's own signature.
let withdraw = ChannelWithdrawOp {
    channel_id,
    outputs: outputs.clone(),
    withdraw_nonce: current_channel_state.withdrawal_nonce,
};
let (tx, msg_id, own_sig) = handle
    .prepare_tx(
        [Op::ChannelWithdraw(withdraw)].into(),
        inscription_payload,
    )
    .await?;

// 2. Every other accredited signer signs `tx` with their own key and
//    returns the signature plus their accredited-key index. Transport
//    is application-defined.
let signatures: Vec<IndexedSignature> = collect_signatures_from_committee(
    &tx,
    IndexedSignature::new(own_key_index, own_sig.clone()),
).await?;

// 3. Once `withdraw_threshold` signatures are gathered, assemble the
//    proof and submit.
let withdraw_proof = ChannelMultiSigProof::new(signatures)?;
let signed_tx = SignedMantleTx::new(
    tx,
    vec![
        OpProof::ChannelMultiSigProof(withdraw_proof),
        OpProof::Ed25519Sig(own_sig),
    ],
)?;
handle.submit_signed_tx(signed_tx, msg_id).await?;
```

### Observing your own withdraws

Withdraws you publish surface twice on the event stream:

1. `Event::Published { tx: PublishedTx::AtomicWithdraw(..) }` — as soon as the transaction is submitted, only if the transaction was submitted by `publish_atomic_withdraw` (single-sequencer zones).
2. `Event::TxsFinalized { items }` containing the matching `tx_hash` with `FinalizedOp::Withdraw` entries, as soon as the block containing the transaction is finalized.

Withdraws submitted by low-level APIs (multi-sequencer zones) only surface in `Event::TxsFinalized`.

```rust
use futures::StreamExt as _;
use lb_zone_sdk::sequencer::{Event, FinalizedOp, ZoneSequencer};

let mut events = sequencer.events();
while let Some(event) = events.next().await {
    if let Event::TxsFinalized { items } = event {
        for tx in items {
            for op in tx.ops {
                if let FinalizedOp::Withdraw(withdrawal) = op {
                    println!(
                        "Withdrawn {:?} in tx {:?}",
                        withdrawal.op, withdrawal.tx_hash,
                    );
                }
            }
        }
    }
}
```

### Reorgs and republish

If a withdraw submitted via `publish_atomic_withdraw` has its parent inscription orphaned by a chain reorg, the Zone SDK fires `Event::ChannelUpdate { orphaned, adopted }` with the abandoned tx in `orphaned`. The original signed transaction is no longer valid. The user must decide whether to republish — re-call `publish_atomic_withdraw` with the same inscription payload and `WithdrawArg`; the Zone SDK refills the inscription parent and the withdrawal nonce from current Bedrock state.

```rust
use futures::StreamExt as _;
use lb_zone_sdk::sequencer::{Event, OrphanedTx, WithdrawArg};

let mut events = sequencer.events();
while let Some(event) = events.next().await {
    if let Event::ChannelUpdate { orphaned, .. } = event {
        for tx in orphaned {
            if let OrphanedTx::AtomicWithdraw(info) = tx {
                let withdraws = info
                    .withdraws
                    .into_iter()
                    .map(|w| WithdrawArg { outputs: w.op.outputs })
                    .collect();
                handle
                    .publish_atomic_withdraw(info.inscription.payload, withdraws)
                    .await
                    .ok();
            }
        }
    }
}
```
