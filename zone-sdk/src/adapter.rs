use std::{collections::HashMap, pin::Pin};

use async_trait::async_trait;
use futures::{Stream, stream};
use lb_common_http_client::{
    ApiBlock, BlockInfo, ChainServiceInfo, CommonHttpClient, Error, Event, EventPayload, Events,
    ProcessedBlockEvent, Slot,
};
use lb_core::{
    crypto::Hash,
    header::HeaderId,
    mantle::{
        Op, SignedMantleTx, Transaction as _, TxHash, Value,
        channel::ChannelState,
        ops::{OpId as _, channel::ChannelId},
    },
};
use lb_http_api_common::queries::BlocksStreamQuery;
use lb_log_targets::zone_sdk;
use reqwest::Url;
use tracing::warn;

use crate::{Deposit, Withdraw, ZoneBlock, ZoneMessage};

const TARGET: &str = zone_sdk::ADAPTER;

/// A boxed, pinned, Send stream.
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

#[async_trait]
pub trait Node {
    async fn consensus_info(&self) -> Result<ChainServiceInfo, Error>;

    async fn channel_state(&self, channel_id: ChannelId) -> Result<Option<ChannelState>, Error>;

    async fn block_stream(&self) -> Result<BoxStream<ProcessedBlockEvent>, Error>;

    async fn blocks_range_stream(
        &self,
        params: BlocksStreamQuery,
    ) -> Result<BoxStream<ProcessedBlockEvent>, Error>;

    async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, Error>;

    async fn block(&self, id: HeaderId) -> Result<Option<ApiBlock>, Error>;

    async fn block_events(&self, id: HeaderId) -> Result<Option<Events>, Error>;

    async fn immutable_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
    ) -> Result<Vec<ApiBlock>, Error>;

    async fn zone_messages_in_block(
        &self,
        id: HeaderId,
        channel_id: ChannelId,
    ) -> Result<BoxStream<ZoneMessage>, Error>;

    async fn zone_messages_in_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
        channel_id: ChannelId,
    ) -> Result<BoxStream<(ZoneMessage, Slot)>, Error>;

    async fn post_transaction(&self, tx: SignedMantleTx) -> Result<(), Error>;
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

    async fn channel_state(&self, channel_id: ChannelId) -> Result<Option<ChannelState>, Error> {
        self.client
            .channel_state(self.base_url.clone(), channel_id)
            .await
    }

    async fn block_stream(&self) -> Result<BoxStream<ProcessedBlockEvent>, Error> {
        let stream = self.client.get_blocks_stream(self.base_url.clone()).await?;
        Ok(Box::pin(stream))
    }

    async fn blocks_range_stream(
        &self,
        params: BlocksStreamQuery,
    ) -> Result<BoxStream<ProcessedBlockEvent>, Error> {
        let stream = self
            .client
            .get_blocks_range_stream(self.base_url.clone(), params)
            .await?;
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

    async fn zone_messages_in_block(
        &self,
        id: HeaderId,
        channel_id: ChannelId,
    ) -> Result<BoxStream<ZoneMessage>, Error> {
        let Some(block) = self
            .client
            .get_block_by_id(self.base_url.clone(), id)
            .await?
        else {
            return Ok(Box::pin(stream::empty()));
        };

        let deposit_amounts = if has_channel_deposit(&block.transactions, channel_id) {
            let events = self
                .client
                .get_block_events(self.base_url.clone(), id)
                .await?
                .unwrap_or_default();
            build_deposit_amounts(&events)
        } else {
            HashMap::new()
        };

        let messages = block_to_messages(block.transactions, channel_id, &deposit_amounts);
        Ok(Box::pin(stream::iter(messages)))
    }

    async fn zone_messages_in_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
        channel_id: ChannelId,
    ) -> Result<BoxStream<(ZoneMessage, Slot)>, Error> {
        let blocks = self
            .client
            .get_immutable_blocks(
                self.base_url.clone(),
                slot_from.into_inner(),
                slot_to.into_inner(),
            )
            .await?;

        let mut all_messages = Vec::new();
        for block in blocks {
            let slot = block.header.slot;
            let deposit_amounts = if has_channel_deposit(&block.transactions, channel_id) {
                let events = self
                    .client
                    .get_block_events(self.base_url.clone(), block.header.id)
                    .await?
                    .unwrap_or_default();
                build_deposit_amounts(&events)
            } else {
                HashMap::new()
            };

            for message in block_to_messages(block.transactions, channel_id, &deposit_amounts) {
                all_messages.push((message, slot));
            }
        }

        Ok(Box::pin(stream::iter(all_messages)))
    }

    async fn post_transaction(&self, tx: SignedMantleTx) -> Result<(), Error> {
        self.client
            .post_transaction(self.base_url.clone(), tx)
            .await
    }
}

/// Returns true if `transactions` contains any deposit op on `channel_id`.
pub(crate) fn has_channel_deposit(transactions: &[SignedMantleTx], channel_id: ChannelId) -> bool {
    transactions.iter().any(|tx| {
        tx.mantle_tx
            .0
            .iter()
            .any(|op| matches!(op, Op::ChannelDeposit(d) if d.channel_id == channel_id))
    })
}

/// Builds a `(tx_hash, op_id) -> amount` lookup from a block's events,
/// keeping only deposit events.
pub(crate) fn build_deposit_amounts(events: &Events) -> HashMap<(TxHash, Hash), Value> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Tx {
                tx_hash,
                op_id,
                payload: EventPayload::Deposit { amount, .. },
            } => Some(((*tx_hash, *op_id), *amount)),
            Event::Ledger(_) => None,
        })
        .collect()
}

/// Walks a block's transactions and emits the [`ZoneMessage`]s relevant to
/// `channel_id`, looking up deposit amounts from `deposit_amounts`.
fn block_to_messages(
    transactions: Vec<SignedMantleTx>,
    channel_id: ChannelId,
    deposit_amounts: &HashMap<(TxHash, Hash), Value>,
) -> Vec<ZoneMessage> {
    transactions
        .into_iter()
        .flat_map(|tx| {
            let tx_hash = tx.hash();
            Vec::from(tx.mantle_tx.0)
                .into_iter()
                .filter_map(move |op| op_to_zone_message(&op, tx_hash, channel_id, deposit_amounts))
        })
        .collect()
}

/// Converts [`Op`] to [`ZoneMessage`] if it belongs to the given channel.
///
/// Returns [`None`] if the op is not relevant for the channel, or if the op
/// is a deposit without a matching event (in which case the deposit is skipped
/// with a warning — the amount is required to be useful to consumers).
fn op_to_zone_message(
    op: &Op,
    tx_hash: TxHash,
    channel_id: ChannelId,
    deposit_amounts: &HashMap<(TxHash, Hash), Value>,
) -> Option<ZoneMessage> {
    match op {
        Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
            Some(ZoneMessage::Block(ZoneBlock {
                id: inscribe.id(),
                data: inscribe.inscription.clone(),
            }))
        }
        Op::ChannelDeposit(deposit) if deposit.channel_id == channel_id => {
            let op_id = deposit.op_id();
            if let Some(&amount) = deposit_amounts.get(&(tx_hash, op_id)) {
                Some(ZoneMessage::Deposit(Deposit {
                    inputs: deposit.inputs.clone(),
                    amount,
                    metadata: deposit.metadata.clone(),
                }))
            } else {
                warn!(
                    target: TARGET,
                    ?tx_hash,
                    ?op_id,
                    "Deposit op has no matching event in block; skipping"
                );
                None
            }
        }
        Op::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => {
            Some(ZoneMessage::Withdraw(Withdraw {
                outputs: withdraw.outputs.clone(),
            }))
        }
        _ => None,
    }
}
