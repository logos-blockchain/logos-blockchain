use futures::{Stream, StreamExt as _};
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient};
use lb_core::mantle::ops::{Op, channel::ChannelId};
use reqwest::Url;
use tracing::warn;

use crate::ZoneBlock;

/// Zone indexer — follows finalized (LIB) blocks and yields zone blocks
/// for a given channel.
pub struct ZoneIndexer {
    channel_id: ChannelId,
    node_url: Url,
    http_client: CommonHttpClient,
}

impl ZoneIndexer {
    #[must_use]
    pub fn new(channel_id: ChannelId, node_url: Url, auth: Option<BasicAuthCredentials>) -> Self {
        Self {
            channel_id,
            node_url,
            http_client: CommonHttpClient::new(auth),
        }
    }

    /// Follow finalized zone blocks.
    pub async fn follow(
        &self,
    ) -> Result<impl Stream<Item = ZoneBlock> + '_, lb_common_http_client::Error> {
        let lib_stream = self
            .http_client
            .get_lib_stream(self.node_url.clone())
            .await?;

        let channel_id = self.channel_id;
        let stream = lib_stream.filter_map(move |block_info| {
            let http_client = self.http_client.clone();
            let node_url = self.node_url.clone();
            let header_id = block_info.header_id;

            async move {
                let block = match http_client.get_block(node_url, header_id).await {
                    Ok(Some(block)) => block,
                    Ok(None) => {
                        warn!("LIB block {header_id} not found");
                        return None;
                    }
                    Err(e) => {
                        warn!("Failed to fetch LIB block {header_id}: {e}");
                        return None;
                    }
                };

                let zone_blocks: Vec<ZoneBlock> = block
                    .transactions()
                    .flat_map(|tx| &tx.mantle_tx.ops)
                    .filter_map(|op| match op {
                        Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id => {
                            Some(ZoneBlock {
                                data: inscribe.inscription.clone(),
                            })
                        }
                        _ => None,
                    })
                    .collect();

                Some(futures::stream::iter(zone_blocks))
            }
        });

        Ok(stream.flatten())
    }
}
