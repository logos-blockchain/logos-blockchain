use async_trait::async_trait;
use futures::Stream;
use lb_common_http_client::{BlockInfo, CommonHttpClient, Error, Slot};
use lb_core::{
    header::HeaderId,
    mantle::{Op, ops::channel::ChannelId},
};
use reqwest::Url;

use crate::{Deposit, ZoneBlock, ZoneMessage};

#[async_trait]
pub trait Node {
    async fn lib_slot(&self) -> Result<Slot, Error>;

    async fn lib_stream(&self) -> Result<impl Stream<Item = BlockInfo>, Error>;

    async fn zone_messages_in_block(
        &self,
        id: HeaderId,
        channel_id: ChannelId,
    ) -> Result<Vec<ZoneMessage>, Error>;

    async fn zone_messages_in_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
        channel_id: ChannelId,
    ) -> Result<Vec<(ZoneMessage, Slot)>, Error>;
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
    // TODO(node-api): expose `lib_slot` in /cryptarchia/info so indexer
    // doesn't need two calls (`consensus_info` + `get_block(lib)`).
    async fn lib_slot(&self) -> Result<Slot, Error> {
        let info = self.client.consensus_info(self.base_url.clone()).await?;
        Ok(self
            .client
            .get_block(self.base_url.clone(), info.lib)
            .await?
            .map_or(
                // Genesis block isn't stored as a regular block
                Slot::genesis(),
                |block| block.header().slot(),
            ))
    }

    async fn lib_stream(&self) -> Result<impl Stream<Item = BlockInfo>, Error> {
        self.client.get_lib_stream(self.base_url.clone()).await
    }

    async fn zone_messages_in_block(
        &self,
        id: HeaderId,
        channel_id: ChannelId,
    ) -> Result<Vec<ZoneMessage>, Error> {
        let Some(block) = self.client.get_block(self.base_url.clone(), id).await? else {
            return Ok(Vec::new());
        };

        Ok(block
            .transactions()
            .flat_map(|tx| &tx.mantle_tx.ops)
            .filter_map(|op| op_to_zone_message(op, channel_id))
            .collect())
    }

    async fn zone_messages_in_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
        channel_id: ChannelId,
    ) -> Result<Vec<(ZoneMessage, Slot)>, Error> {
        let blocks = self
            .client
            .get_blocks(
                self.base_url.clone(),
                slot_from.into_inner(),
                slot_to.into_inner(),
            )
            .await?;

        Ok(blocks
            .iter()
            .flat_map(|block| {
                block
                    .transactions
                    .iter()
                    .flat_map(|tx| &tx.mantle_tx.ops)
                    .filter_map(|op| op_to_zone_message(op, channel_id))
                    .map(|msg| (msg, block.header.slot))
            })
            .collect())
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
