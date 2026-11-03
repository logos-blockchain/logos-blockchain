use std::{collections::HashMap, pin::Pin};

use async_trait::async_trait;
use futures::{Stream, stream};
use lb_common_http_client::{
    ApiBlock, BlockInfo, ChainServiceInfo, CommonHttpClient, Error, Event, Events,
    ProcessedBlockEvent, Slot, TimeInfo, TxEventPayload,
};
use lb_core::{
    crypto::Hash,
    events::TxEvent,
    header::HeaderId,
    mantle::{
        SignedOps,
        channel::ChannelState,
        ledger::verification_mode::{StandardMode, VerificationMode},
        ops::{
            OpId as _, OpRef,
            channel::{ChannelId, deposit::DepositOp},
        },
        traits::Hashable as _,
        transactions::{
            hash::TxHash,
            states::{Unverified, VerificationState},
        },
    },
};
use lb_http_api_common::bodies::wallet::fund::{WalletFundRequestBody, WalletFundResponseBody};
use lb_log_targets::zone_sdk;
use reqwest::Url;
use tracing::warn;

use crate::{Deposit, ZoneBlock, ZoneMessage};

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
    async fn zone_messages_in_block(
        &self,
        id: HeaderId,
        channel_id: ChannelId,
    ) -> Result<BoxStream<ZoneMessage>, Error> {
        let Some(block) = self.block(id).await? else {
            return Ok(Box::pin(stream::empty()));
        };

        let deposit_events = if has_channel_deposit(&block.transactions, channel_id) {
            let events = self.block_events(id).await?.unwrap_or_default();
            build_deposit_events(&events)
        } else {
            HashMap::new()
        };

        let messages = block_to_messages(block.transactions, channel_id, &deposit_events);
        Ok(Box::pin(stream::iter(messages)))
    }

    /// The [`ZoneMessage`]s of `channel_id` in the immutable slot range,
    /// composed from [`immutable_blocks`](Node::immutable_blocks) and
    /// [`block_events`](Node::block_events).
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
            let deposit_events = if has_channel_deposit(&block.transactions, channel_id) {
                let events = self
                    .block_events(block.header.id)
                    .await?
                    .unwrap_or_default();
                build_deposit_events(&events)
            } else {
                HashMap::new()
            };

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

/// Returns true if `transactions` contains any deposit op on `channel_id`.
pub(crate) fn has_channel_deposit<State: VerificationState, Mode: VerificationMode>(
    transactions: &[SignedOps<State, Mode>],
    channel_id: ChannelId,
) -> bool {
    transactions.iter().any(|tx| {
        tx.op_refs()
            .into_iter()
            .any(|op| matches!(op, OpRef::ChannelDeposit(d) if d.channel_id == channel_id))
    })
}

/// Identity of a deposit op within a block: its containing tx and the op id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DepositOpKey {
    pub tx_hash: TxHash,
    pub op_id: Hash,
}

/// Per-block lookup of deposit event payloads by op identity.
pub(crate) type DepositEvents = HashMap<DepositOpKey, TxEventPayload>;

/// Builds the [`DepositEvents`] lookup from a block's events, keeping only
/// deposit events.
pub(crate) fn build_deposit_events(events: &Events) -> DepositEvents {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Tx(TxEvent {
                tx_hash,
                op_id,
                payload: payload @ TxEventPayload::Deposit { .. },
            }) => Some((
                DepositOpKey {
                    tx_hash: *tx_hash,
                    op_id: *op_id,
                },
                payload.clone(),
            )),
            Event::Tx { .. } | Event::Header(_) | Event::Block(_) => None,
        })
        .collect()
}

/// The [`Deposit`] message of `deposit` in `tx_hash`, with the commitments
/// its event reports.
pub(crate) fn deposit_message(
    tx_hash: TxHash,
    deposit: &DepositOp,
    event: &TxEventPayload,
) -> Deposit {
    // `build_deposit_events` keeps deposit payloads only.
    let outputs = if let TxEventPayload::Deposit { outputs, .. } = event {
        outputs
    } else {
        &deposit.outputs
    };
    Deposit {
        tx_hash,
        op_id: deposit.op_id(),
        inputs: deposit.inputs.clone(),
        outputs: outputs.clone(),
        metadata: deposit.metadata.clone(),
    }
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
/// Returns [`None`] if the op is not relevant for the channel, or if the op
/// is a deposit without a matching event (in which case the deposit is skipped
/// with a warning — the amount is required to be useful to consumers).
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
            deposit_events
                .get(&DepositOpKey { tx_hash, op_id })
                .map_or_else(
                    || {
                        warn!(
                            target: TARGET,
                            ?tx_hash,
                            ?op_id,
                            "Deposit op has no matching event in block; skipping"
                        );
                        None
                    },
                    |event| {
                        Some(ZoneMessage::Deposit(deposit_message(
                            tx_hash, deposit, event,
                        )))
                    },
                )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{
        Op,
        notes::{NoteCommitment, NoteNullifier, OutputCommitments},
        ops::channel::deposit::Metadata,
    };
    use lb_groth16::Fr;

    use super::*;
    use crate::test_support::unverified_tx_with_ops;

    fn deposit_op(channel_id: ChannelId, input_seed: u32, metadata: Metadata) -> DepositOp {
        DepositOp {
            channel_id,
            inputs: [NoteNullifier(Fr::from(input_seed))].into(),
            commitment_merkle_root: Fr::from(0u64),
            outputs: [NoteCommitment(Fr::from(input_seed))].into(),
            excess_value: 0,
            metadata,
        }
    }

    fn deposit_event(channel_id: ChannelId, output_seed: u64) -> TxEventPayload {
        TxEventPayload::Deposit {
            channel_id,
            metadata: Metadata::default(),
            outputs: [NoteCommitment(Fr::from(output_seed))].into(),
        }
    }

    /// Each indexer-facing message must carry the identity of the op it was
    /// built from, not a neighbouring op's, and not a neighbouring tx's.
    /// Zone consumers correlate an L2 mint to a finalized L1 deposit by
    /// `(tx_hash, op_id)`, so a crossed identity is silent corruption of the
    /// exactly-once key rather than a visible failure.
    #[test]
    fn block_to_messages_stamps_each_op_with_its_own_identity() {
        let channel_id = ChannelId::from([0; 32]);
        let other_channel = ChannelId::from([9; 32]);

        // tx_a mixes our channel's deposit with a foreign one, so a walker that
        // reused the wrong op's id (or the wrong channel's) would surface here.
        let our_deposit = deposit_op(channel_id, 1, b"to Alice".into());
        let foreign_deposit = deposit_op(other_channel, 2, b"to Bob".into());
        let tx_a = unverified_tx_with_ops(vec![
            Op::ChannelDeposit(our_deposit.clone()),
            Op::ChannelDeposit(foreign_deposit.clone()),
        ]);
        let tx_a_hash = tx_a.hash();

        // A second tx proves `tx_hash` is stamped per-tx, not per-block.
        let later_deposit = deposit_op(channel_id, 3, b"to Carol".into());
        let tx_b = unverified_tx_with_ops(vec![Op::ChannelDeposit(later_deposit.clone())]);
        let tx_b_hash = tx_b.hash();

        // Every deposit gets an event, so channel filtering is proven to be
        // the reason the foreign one is dropped, not a missing event.
        let deposit_events = DepositEvents::from([
            (
                DepositOpKey {
                    tx_hash: tx_a_hash,
                    op_id: our_deposit.op_id(),
                },
                deposit_event(channel_id, 1234),
            ),
            (
                DepositOpKey {
                    tx_hash: tx_a_hash,
                    op_id: foreign_deposit.op_id(),
                },
                deposit_event(other_channel, 999),
            ),
            (
                DepositOpKey {
                    tx_hash: tx_b_hash,
                    op_id: later_deposit.op_id(),
                },
                deposit_event(channel_id, 5678),
            ),
        ]);

        let messages = block_to_messages(vec![tx_a, tx_b], channel_id, &deposit_events);

        assert_eq!(
            messages.len(),
            2,
            "one deposit on our channel in tx_a, one in tx_b"
        );
        match &messages[0] {
            ZoneMessage::Deposit(deposit) => {
                assert_eq!(deposit.tx_hash, tx_a_hash);
                assert_eq!(deposit.op_id, our_deposit.op_id());
                assert_eq!(deposit.inputs, our_deposit.inputs);
                assert_eq!(deposit.outputs, [NoteCommitment(Fr::from(1234u64))].into());
                assert_ne!(
                    deposit.op_id,
                    foreign_deposit.op_id(),
                    "deposit must not inherit its tx-mate's identity"
                );
            }
            other @ ZoneMessage::Block(_) => panic!("expected Deposit, got {other:?}"),
        }
        match &messages[1] {
            ZoneMessage::Deposit(deposit) => {
                assert_eq!(deposit.tx_hash, tx_b_hash);
                assert_eq!(deposit.op_id, later_deposit.op_id());
                assert_eq!(deposit.outputs, [NoteCommitment(Fr::from(5678u64))].into());
            }
            other @ ZoneMessage::Block(_) => panic!("expected Deposit, got {other:?}"),
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

    /// A deposit whose amount is absent from the block's events is dropped
    /// entirely, taking its identity with it — consumers never see a deposit
    /// they cannot value.
    #[test]
    fn block_to_messages_skips_deposit_without_matching_event() {
        let channel_id = ChannelId::from([0; 32]);
        let tx = unverified_tx_with_ops(vec![Op::ChannelDeposit(deposit_op(
            channel_id,
            1,
            b"to Alice".into(),
        ))]);

        let messages = block_to_messages(vec![tx], channel_id, &HashMap::new());

        assert!(messages.is_empty(), "deposit without an event is skipped");
    }

    #[test]
    fn deposit_message_carries_the_commitments_of_the_event() {
        let deposit = DepositOp::sample();
        let outputs: OutputCommitments = deposit.outputs.clone();
        let message = deposit_message(
            TxHash::from([1; 32]),
            &deposit,
            &TxEventPayload::Deposit {
                channel_id: deposit.channel_id,
                metadata: deposit.metadata.clone(),
                outputs: outputs.clone(),
            },
        );
        assert_eq!(message.outputs, outputs);
        assert_eq!(message.inputs, deposit.inputs);
    }
}
