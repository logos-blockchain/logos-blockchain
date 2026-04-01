use async_trait::async_trait;
use futures::{Stream, stream};
use lb_common_http_client::{BlockInfo, CommonHttpClient, CryptarchiaInfo, Error, Slot};
use lb_core::{
    block::Block,
    header::HeaderId,
    mantle::{Op, ops::channel::ChannelId},
};
use reqwest::Url;

use crate::{Deposit, ZoneBlock, ZoneMessage};

#[async_trait]
pub trait Node {
    async fn consensus_info(&self) -> Result<CryptarchiaInfo, Error>;

    async fn lib_stream(&self) -> Result<impl Stream<Item = BlockInfo>, Error>;

    async fn zone_messages_in_block(
        &self,
        id: HeaderId,
        channel_id: ChannelId,
    ) -> Result<impl Stream<Item = ZoneMessage>, Error>;

    async fn zone_messages_in_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
        channel_id: ChannelId,
    ) -> Result<impl Stream<Item = (ZoneMessage, Slot)>, Error>;
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
    async fn consensus_info(&self) -> Result<CryptarchiaInfo, Error> {
        self.client.consensus_info(self.base_url.clone()).await
    }

    async fn lib_stream(&self) -> Result<impl Stream<Item = BlockInfo>, Error> {
        self.client.get_lib_stream(self.base_url.clone()).await
    }

    async fn zone_messages_in_block(
        &self,
        id: HeaderId,
        channel_id: ChannelId,
    ) -> Result<impl Stream<Item = ZoneMessage>, Error> {
        let transactions = self
            .client
            .get_block(self.base_url.clone(), id)
            .await?
            .map_or_else(|| Vec::with_capacity(0), Block::into_transactions);

        Ok(stream::iter(
            transactions
                .into_iter()
                .flat_map(|tx| tx.mantle_tx.ops)
                .filter_map(move |op| op_to_zone_message(&op, channel_id)),
        ))
    }

    async fn zone_messages_in_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
        channel_id: ChannelId,
    ) -> Result<impl Stream<Item = (ZoneMessage, Slot)>, Error> {
        let blocks = self
            .client
            .get_blocks(
                self.base_url.clone(),
                slot_from.into_inner(),
                slot_to.into_inner(),
            )
            .await?;

        Ok(stream::iter(blocks.into_iter().flat_map(move |block| {
            let slot = block.header.slot;
            block
                .transactions
                .into_iter()
                .flat_map(|tx| tx.mantle_tx.ops)
                .filter_map(move |op| op_to_zone_message(&op, channel_id))
                .map(move |msg| (msg, slot))
        })))
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
                amount: deposit.amount,
                metadata: deposit.metadata.clone(),
            }))
        }
        _ => None,
    }
}
