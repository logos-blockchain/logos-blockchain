use std::{num::NonZero, pin::Pin};

use async_trait::async_trait;
use futures::{Stream, stream};
use lb_common_http_client::{
    ApiBlock, BlockInfo, ChainServiceInfo, CommonHttpClient, Error, ProcessedBlockEvent, Slot,
};
use lb_core::{
    header::HeaderId,
    mantle::{Op, SignedMantleTx, ops::channel::ChannelId},
};
use reqwest::Url;

use crate::{Deposit, Withdraw, ZoneBlock, ZoneMessage};

/// A boxed, pinned, Send stream.
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

#[async_trait]
pub trait Node {
    async fn consensus_info(&self) -> Result<ChainServiceInfo, Error>;

    async fn block_stream(&self) -> Result<BoxStream<ProcessedBlockEvent>, Error>;

    async fn blocks_range_stream(
        &self,
        blocks_limit: Option<NonZero<usize>>,
        slot_from: Option<u64>,
        slot_to: Option<u64>,
        descending: Option<bool>,
        server_batch_size: Option<NonZero<usize>>,
        immutable_only: Option<bool>,
    ) -> Result<BoxStream<ProcessedBlockEvent>, Error>;

    async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, Error>;

    async fn block(&self, id: HeaderId) -> Result<Option<ApiBlock>, Error>;

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

    async fn block_stream(&self) -> Result<BoxStream<ProcessedBlockEvent>, Error> {
        let stream = self.client.get_blocks_stream(self.base_url.clone()).await?;
        Ok(Box::pin(stream))
    }

    async fn blocks_range_stream(
        &self,
        blocks_limit: Option<NonZero<usize>>,
        slot_from: Option<u64>,
        slot_to: Option<u64>,
        descending: Option<bool>,
        server_batch_size: Option<NonZero<usize>>,
        immutable_only: Option<bool>,
    ) -> Result<BoxStream<ProcessedBlockEvent>, Error> {
        let stream = self
            .client
            .get_blocks_range_stream(
                self.base_url.clone(),
                blocks_limit,
                slot_from,
                slot_to,
                descending,
                server_batch_size,
                immutable_only,
            )
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
        let transactions = self
            .client
            .get_block_by_id(self.base_url.clone(), id)
            .await?
            .map_or_else(|| Vec::with_capacity(0), |block| block.transactions);

        Ok(Box::pin(stream::iter(
            transactions
                .into_iter()
                .flat_map(|tx| tx.mantle_tx.0)
                .filter_map(move |op| op_to_zone_message(&op, channel_id)),
        )))
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

        Ok(Box::pin(stream::iter(blocks.into_iter().flat_map(
            move |block| {
                let slot = block.header.slot;
                block
                    .transactions
                    .into_iter()
                    .flat_map(|tx| tx.mantle_tx.0)
                    .filter_map(move |op| op_to_zone_message(&op, channel_id))
                    .map(move |msg| (msg, slot))
            },
        ))))
    }

    async fn post_transaction(&self, tx: SignedMantleTx) -> Result<(), Error> {
        self.client
            .post_transaction(self.base_url.clone(), tx)
            .await
    }
}

/// Converts [`Op`] to [`ZoneMessage`] if it belongs to the given channel.
///
/// Returns [`None`] if the op is not relevant for the channel.
fn op_to_zone_message(op: &Op, channel_id: ChannelId) -> Option<ZoneMessage> {
    match op {
        Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
            Some(ZoneMessage::Block(ZoneBlock {
                id: inscribe.id(),
                data: inscribe.inscription.clone(),
            }))
        }
        Op::ChannelDeposit(deposit) if deposit.channel_id == channel_id => {
            Some(ZoneMessage::Deposit(Deposit {
                inputs: deposit.inputs.clone(),
                metadata: deposit.metadata.clone(),
            }))
        }
        Op::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => {
            Some(ZoneMessage::Withdraw(Withdraw {
                outputs: withdraw.outputs.clone(),
            }))
        }
        _ => None,
    }
}
