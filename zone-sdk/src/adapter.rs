use std::{collections::HashMap, pin::Pin};

use async_trait::async_trait;
use futures::{Stream, stream};
use lb_common_http_client::{
    ApiBlock, BlockInfo, ChainServiceInfo, CommonHttpClient, Error, Event, Events,
    ProcessedBlockEvent, Slot, TimeInfo, TxEventPayload,
};
use lb_core::{
    crypto::Hash,
    events::{DepositRecreatedNotes, TxEvent},
    header::HeaderId,
    mantle::{
        SignedOps, Value,
        channel::ChannelState,
        ledger::verification_mode::{StandardMode, VerificationMode},
        ops::{OpId as _, OpRef, channel::ChannelId},
        traits::Hashable as _,
        transactions::{
            hash::TxHash,
            states::{Unverified, VerificationState},
        },
    },
};
use lb_http_api_common::bodies::{
    mantle::LimitsResponseBody,
    wallet::fund::{WalletFundRequestBody, WalletFundResponseBody},
};
use lb_log_targets::zone_sdk;
use reqwest::Url;
use tracing::error;

use crate::{Deposit, Withdraw, ZoneBlock, ZoneMessage};

const TARGET: &str = zone_sdk::ADAPTER;

/// A boxed, pinned, Send stream.
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

/// Backend interface of the SDK — every chain access goes through this trait.
///
/// [`NodeHttpClient`] implements it over the node's HTTP API. Custom backends
/// (e.g. an embedded node reached over a different transport) implement this
/// trait externally and are passed to the generic entry point
/// [`ZoneSequencer::init`](crate::sequencer::ZoneSequencer::init) — no
/// registration beyond the `impl` is needed.
///
/// Only the primitive methods — each a single node API call — must be
/// implemented. The `zone_messages_*` methods are compositions of those
/// primitives and come with default implementations; override them only to
/// serve the same message vocabulary more efficiently.
///
/// The entry point additionally requires `Clone + Send + Sync + 'static` on
/// the implementation.
///
/// Every foreign type appearing in these signatures is re-exported in
/// [`crate::node_types`], so an implementation only needs to depend on this
/// crate.
#[async_trait]
pub trait Node {
    async fn consensus_info(&self) -> Result<ChainServiceInfo, Error>;

    async fn time_info(&self) -> Result<TimeInfo, Error>;

    async fn limits(&self) -> Result<LimitsResponseBody, Error>;

    async fn channel_state(&self, channel_id: ChannelId) -> Result<Option<ChannelState>, Error>;

    async fn block_stream(&self) -> Result<BoxStream<ProcessedBlockEvent>, Error>;

    async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, Error>;

    async fn block(&self, id: HeaderId) -> Result<Option<ApiBlock>, Error>;

    async fn block_events(&self, id: HeaderId) -> Result<Option<Events>, Error>;

    async fn immutable_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
    ) -> Result<Vec<ApiBlock>, Error>;

    async fn post_transaction(&self, tx: SignedOps<Unverified, StandardMode>) -> Result<(), Error>;

    /// Fund a transaction from the node's wallet.
    ///
    /// The node adds fee inputs and change from its own wallet, signs only
    /// the appended fee transfer, and returns the funded — still unsigned —
    /// transaction together with the transfer proof.
    async fn fund_tx(
        &self,
        request: WalletFundRequestBody,
    ) -> Result<WalletFundResponseBody, Error>;

    /// The [`ZoneMessage`]s of `channel_id` carried by block `id`, composed
    /// from [`block`](Node::block) and [`block_events`](Node::block_events).
    ///
    /// Fails if the node serves no event for a deposit of the block, rather
    /// than leave the deposit out.
    async fn zone_messages_in_block(
        &self,
        id: HeaderId,
        channel_id: ChannelId,
    ) -> Result<BoxStream<ZoneMessage>, Error> {
        let Some(block) = self.block(id).await? else {
            return Ok(Box::pin(stream::empty()));
        };

        let deposit_events =
            fetch_block_deposit_events(self, id, &block.transactions, channel_id).await?;

        let messages = block_to_messages(block.transactions, channel_id, &deposit_events);
        Ok(Box::pin(stream::iter(messages)))
    }

    /// The [`ZoneMessage`]s of `channel_id` in the immutable slot range,
    /// composed from [`immutable_blocks`](Node::immutable_blocks) and
    /// [`block_events`](Node::block_events).
    ///
    /// Fails if the node serves no event for a deposit in the range, rather
    /// than leave the deposit out.
    async fn zone_messages_in_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
        channel_id: ChannelId,
    ) -> Result<BoxStream<(ZoneMessage, Slot)>, Error> {
        let blocks = self.immutable_blocks(slot_from, slot_to).await?;

        let mut all_messages = Vec::new();
        for block in blocks {
            let slot = block.header.slot;
            let deposit_events =
                fetch_block_deposit_events(self, block.header.id, &block.transactions, channel_id)
                    .await?;

            for message in block_to_messages(block.transactions, channel_id, &deposit_events) {
                all_messages.push((message, slot));
            }
        }

        Ok(Box::pin(stream::iter(all_messages)))
    }
}

#[derive(Clone)]
pub struct NodeHttpClient {
    client: CommonHttpClient,
    base_url: Url,
}

impl NodeHttpClient {
    #[must_use]
    pub const fn new(client: CommonHttpClient, base_url: Url) -> Self {
        Self { client, base_url }
    }
}

#[async_trait]
impl Node for NodeHttpClient {
    async fn consensus_info(&self) -> Result<ChainServiceInfo, Error> {
        self.client.consensus_info(self.base_url.clone()).await
    }

    async fn time_info(&self) -> Result<TimeInfo, Error> {
        self.client.time_info(self.base_url.clone()).await
    }

    async fn limits(&self) -> Result<LimitsResponseBody, Error> {
        self.client.limits(self.base_url.clone()).await
    }

    async fn channel_state(&self, channel_id: ChannelId) -> Result<Option<ChannelState>, Error> {
        self.client
            .channel_state(self.base_url.clone(), channel_id)
            .await
    }

    async fn block_stream(&self) -> Result<BoxStream<ProcessedBlockEvent>, Error> {
        let stream = self.client.get_blocks_stream(self.base_url.clone()).await?;
        Ok(Box::pin(stream))
    }

    async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, Error> {
        let stream = self.client.get_lib_stream(self.base_url.clone()).await?;
        Ok(Box::pin(stream))
    }

    async fn block(&self, id: HeaderId) -> Result<Option<ApiBlock>, Error> {
        self.client.get_block_by_id(self.base_url.clone(), id).await
    }

    async fn block_events(&self, id: HeaderId) -> Result<Option<Events>, Error> {
        self.client
            .get_block_events(self.base_url.clone(), id)
            .await
    }

    async fn immutable_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
    ) -> Result<Vec<ApiBlock>, Error> {
        self.client
            .get_immutable_blocks(
                self.base_url.clone(),
                slot_from.into_inner(),
                slot_to.into_inner(),
            )
            .await
    }

    async fn post_transaction(&self, tx: SignedOps<Unverified, StandardMode>) -> Result<(), Error> {
        self.client
            .post_transaction(self.base_url.clone(), tx)
            .await
    }

    async fn fund_tx(
        &self,
        request: WalletFundRequestBody,
    ) -> Result<WalletFundResponseBody, Error> {
        self.client.fund_tx(self.base_url.clone(), request).await
    }
}

/// Identity of a deposit op within a block: its containing tx and the op id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DepositOpKey {
    pub tx_hash: TxHash,
    pub op_id: Hash,
}

/// A deposit's block-event payload: the deposited total and the channel
/// notes the deposit re-created.
#[derive(Debug, Clone)]
pub(crate) struct DepositEvent {
    pub amount: Value,
    pub notes: DepositRecreatedNotes,
}

/// Per-block lookup of deposit events by op identity.
pub(crate) type DepositEvents = HashMap<DepositOpKey, DepositEvent>;

/// Builds the [`DepositEvents`] lookup from a block's events, keeping only
/// deposit events.
pub(crate) fn build_deposit_events(events: &Events) -> DepositEvents {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Tx(TxEvent {
                tx_hash,
                op_id,
                payload: TxEventPayload::Deposit { amount, notes, .. },
            }) => Some((
                DepositOpKey {
                    tx_hash: *tx_hash,
                    op_id: *op_id,
                },
                DepositEvent {
                    amount: *amount,
                    notes: notes.clone(),
                },
            )),
            Event::Tx { .. } | Event::Header(_) => None,
        })
        .collect()
}

/// Fetch the deposit-amount lookup for a single block, gated on whether the
/// block has any deposit op for our channel.
///
/// Per node semantics, a block and its events are atomically visible — so a
/// block containing a deposit op must yield an event for that op. The
/// returned `HashMap` is therefore the *complete* `(tx_hash, op_id) → amount`
/// lookup for every deposit op of our channel in this block.
///
/// On any failure (HTTP error, `Ok(None)`, or events missing an entry for
/// some deposit op) we log at error level and return an error. The caller's
/// contract is "either retry, or abandon this block" — never silently emit a
/// partial result, because that drops real deposits.
pub(crate) async fn fetch_block_deposit_events<N>(
    node: &N,
    block_id: HeaderId,
    transactions: &[SignedOps<Unverified, StandardMode>],
    channel_id: ChannelId,
) -> Result<DepositEvents, Error>
where
    N: Node + Sync + ?Sized,
{
    let expected: Vec<DepositOpKey> = transactions
        .iter()
        .flat_map(|tx| {
            let tx_hash = tx.op_refs().hash();
            tx.op_refs().into_iter().filter_map(move |op| match op {
                OpRef::ChannelDeposit(d) if d.channel_id == channel_id => Some(DepositOpKey {
                    tx_hash,
                    op_id: d.op_id(),
                }),
                _ => None,
            })
        })
        .collect();

    if expected.is_empty() {
        return Ok(DepositEvents::new());
    }

    let events = match node.block_events(block_id).await {
        Ok(Some(events)) => events,
        Ok(None) => {
            error!(
                target: TARGET,
                ?block_id,
                "Events endpoint returned no body for a block with a channel deposit; \
                 events should be atomically visible with the block"
            );
            return Err(Error::Server(format!(
                "no events for block {block_id} containing channel deposits"
            )));
        }
        Err(err) => {
            error!(target: TARGET, ?block_id, ?err, "Failed to fetch events for block");
            return Err(err);
        }
    };

    let deposit_events = build_deposit_events(&events);
    for key in &expected {
        if !deposit_events.contains_key(key) {
            error!(
                target: TARGET,
                ?block_id,
                tx_hash = ?key.tx_hash,
                op_id = ?key.op_id,
                "Block events missing an entry for a known channel deposit op; \
                 expected atomic block/events visibility per node semantics"
            );
            return Err(Error::Server(format!(
                "block {block_id} events missing deposit entry for tx {:?} op {:?}",
                key.tx_hash, key.op_id
            )));
        }
    }
    Ok(deposit_events)
}

/// Walks a block's transactions and emits the [`ZoneMessage`]s relevant to
/// `channel_id`, looking up deposit amounts and notes from `deposit_events`.
fn block_to_messages<State: VerificationState, Mode: VerificationMode>(
    transactions: Vec<SignedOps<State, Mode>>,
    channel_id: ChannelId,
    deposit_events: &DepositEvents,
) -> Vec<ZoneMessage> {
    transactions
        .into_iter()
        .flat_map(|tx| {
            let tx_hash = tx.hash();
            tx.op_refs()
                .into_iter()
                .filter_map(move |op| op_to_zone_message(op, tx_hash, channel_id, deposit_events))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Converts [`Op`] to [`ZoneMessage`] if it belongs to the given channel.
///
/// Returns [`None`] if the op is not relevant for the channel.
///
/// # Panics
///
/// If the op is a deposit without an entry in `deposit_events`, which
/// [`fetch_block_deposit_events`] guarantees.
fn op_to_zone_message(
    op: OpRef,
    tx_hash: TxHash,
    channel_id: ChannelId,
    deposit_events: &DepositEvents,
) -> Option<ZoneMessage> {
    match op {
        OpRef::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
            Some(ZoneMessage::Block(ZoneBlock {
                id: inscribe.id(),
                data: inscribe.inscription.clone(),
                signer: inscribe.signer,
            }))
        }
        OpRef::ChannelDeposit(deposit) if deposit.channel_id == channel_id => {
            let op_id = deposit.op_id();
            let event = deposit_events.get(&DepositOpKey { tx_hash, op_id }).expect(
                "deposit_events must contain every channel deposit op - \
                 fetch_block_deposit_events invariant",
            );
            Some(ZoneMessage::Deposit(Deposit {
                tx_hash,
                op_id,
                inputs: deposit.inputs.clone(),
                notes: event.notes.clone(),
                amount: event.amount,
                metadata: deposit.metadata.clone(),
            }))
        }
        OpRef::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => {
            Some(ZoneMessage::Withdraw(Withdraw {
                tx_hash,
                op_id: withdraw.op_id(),
                inputs: withdraw.inputs.clone(),
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;
    use lb_core::mantle::{
        NoteId, Op,
        ledger::BoundedInputs,
        ops::channel::{
            deposit::{DepositOp, Metadata},
            withdraw::ChannelWithdrawOp,
        },
    };
    use lb_groth16::Fr;

    use super::*;
    use crate::test_support::{MockNode, api_block, deposit_event, unverified_tx_with_ops};

    fn deposit_op(channel_id: ChannelId, input_seed: u32, metadata: Metadata) -> DepositOp {
        DepositOp {
            channel_id,
            inputs: BoundedInputs::from(NoteId::from(Fr::from(input_seed))).into(),
            metadata,
        }
    }

    /// A withdraw op is identified by `hash(channel_id || inputs)` — it
    /// carries no nonce — so distinct `input_seed`s are what give two
    /// withdraws distinct `op_id`s. That mirrors the chain, where the notes
    /// being released are spent-once.
    fn withdraw_op(channel_id: ChannelId, input_seed: u32) -> ChannelWithdrawOp {
        ChannelWithdrawOp {
            channel_id,
            inputs: BoundedInputs::from(NoteId::from(Fr::from(input_seed))).into(),
        }
    }

    /// Each indexer-facing message must carry the identity of the op it was
    /// built from — not a neighbouring op's, and not a neighbouring tx's.
    /// Zone consumers correlate an L2 mint to a finalized L1 deposit by
    /// `(tx_hash, op_id)`, so a crossed identity is silent corruption of the
    /// exactly-once key rather than a visible failure.
    #[test]
    fn block_to_messages_stamps_each_op_with_its_own_identity() {
        let channel_id = ChannelId::from([0; 32]);
        let other_channel = ChannelId::from([9; 32]);

        // tx_a mixes three ops so a walker that reused the wrong op's id
        // (or the wrong channel's) would surface here.
        let our_deposit = deposit_op(channel_id, 1, Metadata::new_unchecked(b"to Alice".to_vec()));
        let foreign_deposit = deposit_op(
            other_channel,
            2,
            Metadata::new_unchecked(b"to Bob".to_vec()),
        );
        let our_withdraw = withdraw_op(channel_id, 42);
        let tx_a = unverified_tx_with_ops(vec![
            Op::ChannelDeposit(our_deposit.clone()),
            Op::ChannelDeposit(foreign_deposit.clone()),
            Op::ChannelWithdraw(our_withdraw.clone()),
        ]);
        let tx_a_hash = tx_a.hash();

        // A second tx proves `tx_hash` is stamped per-tx, not per-block.
        let later_withdraw = withdraw_op(channel_id, 7);
        let tx_b = unverified_tx_with_ops(vec![Op::ChannelWithdraw(later_withdraw.clone())]);
        let tx_b_hash = tx_b.hash();

        // Both deposits get an event, so channel filtering is proven to be
        // the reason the foreign one is dropped — not a missing amount.
        let deposit_events = DepositEvents::from([
            (
                DepositOpKey {
                    tx_hash: tx_a_hash,
                    op_id: our_deposit.op_id(),
                },
                DepositEvent {
                    amount: 1234,
                    notes: DepositRecreatedNotes::default(),
                },
            ),
            (
                DepositOpKey {
                    tx_hash: tx_a_hash,
                    op_id: foreign_deposit.op_id(),
                },
                DepositEvent {
                    amount: 999,
                    notes: DepositRecreatedNotes::default(),
                },
            ),
        ]);

        let messages = block_to_messages(vec![tx_a, tx_b], channel_id, &deposit_events);

        assert_eq!(
            messages.len(),
            3,
            "two ops on our channel in tx_a, one in tx_b"
        );
        match &messages[0] {
            ZoneMessage::Deposit(deposit) => {
                assert_eq!(deposit.tx_hash, tx_a_hash);
                assert_eq!(deposit.op_id, our_deposit.op_id());
                assert_eq!(deposit.amount, 1234);
                assert_eq!(deposit.inputs, our_deposit.inputs);
                assert_ne!(
                    deposit.op_id,
                    our_withdraw.op_id(),
                    "deposit must not inherit its tx-mate's identity"
                );
            }
            other => panic!("expected Deposit, got {other:?}"),
        }
        match &messages[1] {
            ZoneMessage::Withdraw(withdraw) => {
                assert_eq!(withdraw.tx_hash, tx_a_hash);
                assert_eq!(withdraw.op_id, our_withdraw.op_id());
                assert_eq!(withdraw.inputs, our_withdraw.inputs);
            }
            other => panic!("expected Withdraw, got {other:?}"),
        }
        match &messages[2] {
            ZoneMessage::Withdraw(withdraw) => {
                assert_eq!(withdraw.tx_hash, tx_b_hash);
                assert_eq!(withdraw.op_id, later_withdraw.op_id());
            }
            other => panic!("expected Withdraw, got {other:?}"),
        }
    }

    /// Pins the public backend contract: an external [`Node`] implementation
    /// (here [`MockNode`], which is not [`NodeHttpClient`]) satisfies the
    /// bounds of the generic entry point. If a bound is ever added to the
    /// constructor, this stops compiling and the trait docs must be updated
    /// alongside it.
    #[tokio::test]
    async fn external_backend_plugs_into_sequencer() {
        use lb_key_management_system_service::keys::Ed25519Key;

        use crate::{
            sequencer::ZoneSequencer,
            test_support::{MockNode, funding_config},
        };

        let channel_id = ChannelId::from([0; 32]);
        let node = MockNode::default();

        let _sequencer = ZoneSequencer::init(
            channel_id,
            Ed25519Key::from_bytes(&[0; 32]),
            node,
            funding_config(),
            None,
        );
    }

    /// The walker contract: `deposit_events` must contain an entry for every
    /// channel-deposit op, as [`fetch_block_deposit_events`] validates. A
    /// panic surfaces a caller violating that — a silent skip would drop a
    /// real deposit.
    #[test]
    #[should_panic(expected = "fetch_block_deposit_events invariant")]
    fn block_to_messages_panics_if_deposit_events_incomplete() {
        let channel_id = ChannelId::from([0; 32]);
        let tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(deposit_op(
            channel_id,
            1,
            Metadata::new_unchecked(b"to Alice".to_vec()),
        ))]);

        drop(block_to_messages(vec![tx], channel_id, &HashMap::new()));
    }

    /// Serves only the primitive calls of a [`MockNode`], so the
    /// `zone_messages_*` defaults run.
    struct PrimitivesOnly(MockNode);

    #[async_trait]
    impl Node for PrimitivesOnly {
        async fn consensus_info(&self) -> Result<ChainServiceInfo, Error> {
            self.0.consensus_info().await
        }

        async fn time_info(&self) -> Result<TimeInfo, Error> {
            self.0.time_info().await
        }

        async fn limits(&self) -> Result<LimitsResponseBody, Error> {
            self.0.limits().await
        }

        async fn channel_state(
            &self,
            channel_id: ChannelId,
        ) -> Result<Option<ChannelState>, Error> {
            self.0.channel_state(channel_id).await
        }

        async fn block_stream(&self) -> Result<BoxStream<ProcessedBlockEvent>, Error> {
            self.0.block_stream().await
        }

        async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, Error> {
            self.0.lib_stream().await
        }

        async fn block(&self, id: HeaderId) -> Result<Option<ApiBlock>, Error> {
            self.0.block(id).await
        }

        async fn block_events(&self, id: HeaderId) -> Result<Option<Events>, Error> {
            self.0.block_events(id).await
        }

        async fn immutable_blocks(
            &self,
            slot_from: Slot,
            slot_to: Slot,
        ) -> Result<Vec<ApiBlock>, Error> {
            self.0.immutable_blocks(slot_from, slot_to).await
        }

        async fn post_transaction(
            &self,
            tx: SignedOps<Unverified, StandardMode>,
        ) -> Result<(), Error> {
            self.0.post_transaction(tx).await
        }

        async fn fund_tx(
            &self,
            request: WalletFundRequestBody,
        ) -> Result<WalletFundResponseBody, Error> {
            self.0.fund_tx(request).await
        }
    }

    /// A node holding `block` both as an immutable block and by id, with
    /// `events` for it, if any.
    fn node_with_block(block: ApiBlock, events: Option<Events>) -> PrimitivesOnly {
        PrimitivesOnly(MockNode {
            events: events
                .map(|events| HashMap::from([(block.header.id, events)]))
                .unwrap_or_default(),
            blocks: vec![block.clone()],
            immutable: vec![block],
            ..MockNode::default()
        })
    }

    #[tokio::test]
    async fn zone_messages_in_blocks_report_a_deposit_with_its_event() {
        let channel_id = ChannelId::from([0; 32]);
        let op = deposit_op(channel_id, 1, Metadata::new_unchecked(b"to Alice".to_vec()));
        let tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(op.clone())]);
        let events = deposit_event(&tx, &op, 1234, Vec::new());
        let node = node_with_block(api_block(1, 0, 1, vec![tx]), Some(events));

        let messages: Vec<_> = node
            .zone_messages_in_blocks(Slot::genesis(), 1.into(), channel_id)
            .await
            .expect("events are served")
            .collect()
            .await;

        assert!(
            matches!(
                messages.as_slice(),
                [(ZoneMessage::Deposit(deposit), slot)]
                    if deposit.op_id == op.op_id() && deposit.amount == 1234 && *slot == 1.into()
            ),
            "{messages:?}"
        );
    }

    /// Leaving the deposit out would lose it for good: the caller moves past
    /// the range.
    #[tokio::test]
    async fn zone_messages_in_blocks_fail_without_the_block_events() {
        let channel_id = ChannelId::from([0; 32]);
        let op = deposit_op(channel_id, 1, Metadata::new_unchecked(b"to Alice".to_vec()));
        let tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(op)]);
        let node = node_with_block(api_block(1, 0, 1, vec![tx]), None);

        assert!(
            node.zone_messages_in_blocks(Slot::genesis(), 1.into(), channel_id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn zone_messages_in_block_fail_without_an_event_of_each_deposit() {
        let channel_id = ChannelId::from([0; 32]);
        let first = deposit_op(channel_id, 1, Metadata::new_unchecked(b"to Alice".to_vec()));
        let second = deposit_op(channel_id, 2, Metadata::new_unchecked(b"to Bob".to_vec()));
        let tx = unverified_tx_with_ops(vec![
            Op::ChannelDeposit(first.clone()),
            Op::ChannelDeposit(second),
        ]);
        let events = deposit_event(&tx, &first, 1234, Vec::new());
        let block = api_block(1, 0, 1, vec![tx]);
        let id = block.header.id;
        let node = node_with_block(block, Some(events));

        assert!(node.zone_messages_in_block(id, channel_id).await.is_err());
    }
}
