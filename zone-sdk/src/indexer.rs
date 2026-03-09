use futures::{Stream, StreamExt as _};
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient};
use lb_core::mantle::ops::{
    Op,
    channel::{ChannelId, MsgId},
};
use reqwest::Url;
use tracing::warn;

use crate::ZoneBlock;

/// Indexer errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] lb_common_http_client::Error),
}

/// Opaque cursor for pagination. Pass to `next_messages` to resume.
///
/// Serializable so that callers can persist it for crash recovery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cursor {
    slot: u64,
    last_id: Option<MsgId>,
}

/// Result of polling for messages.
pub struct PollResult {
    /// Messages found.
    pub messages: Vec<ZoneBlock>,
    /// Cursor to pass to the next `next_messages` call.
    pub cursor: Cursor,
}

/// Zone indexer — reads finalized zone messages from a channel.
pub struct ZoneIndexer {
    channel_id: ChannelId,
    node_url: Url,
    http_client: CommonHttpClient,
}

const BATCH_SIZE: u64 = 100;

impl ZoneIndexer {
    #[must_use]
    pub fn new(channel_id: ChannelId, node_url: Url, auth: Option<BasicAuthCredentials>) -> Self {
        Self {
            channel_id,
            node_url,
            http_client: CommonHttpClient::new(auth),
        }
    }

    /// Subscribe to live zone messages as they finalize.
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
                                id: inscribe.id(),
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

    /// Fetch the LIB slot.
    ///
    /// TODO(node-api): expose `lib_slot` in /cryptarchia/info so indexer
    /// doesn't need two calls (`consensus_info` + `get_block(lib)`).
    async fn lib_slot(&self) -> Result<u64, Error> {
        let info = self
            .http_client
            .consensus_info(self.node_url.clone())
            .await?;

        // Genesis block isn't stored as a regular block, so None here means slot 0.
        Ok(self
            .http_client
            .get_block(self.node_url.clone(), info.lib)
            .await?
            .map_or(0, |block| block.header().slot().into()))
    }

    /// Poll for the next batch of messages.
    ///
    /// Returns up to `limit` messages and a cursor for the next call.
    /// Pass `None` to start from the beginning, or pass the returned cursor
    /// to continue where you left off.
    pub async fn next_messages(
        &self,
        cursor: Option<Cursor>,
        limit: usize,
    ) -> Result<PollResult, Error> {
        let lib_slot = self.lib_slot().await?;

        let (cursor, mut current_slot) = cursor.map_or_else(
            || (Cursor::default(), 0),
            |c| {
                let start = if c.last_id.is_some() {
                    c.slot
                } else {
                    c.slot.saturating_add(1)
                };
                (c, start)
            },
        );

        if current_slot > lib_slot || limit == 0 {
            return Ok(PollResult {
                messages: Vec::new(),
                cursor: Cursor {
                    slot: lib_slot,
                    last_id: None,
                },
            });
        }

        let mut scan = ScanState::new(cursor, limit);

        while current_slot <= lib_slot {
            let end_slot = (current_slot + BATCH_SIZE - 1).min(lib_slot);
            let blocks = self
                .http_client
                .get_blocks(self.node_url.clone(), current_slot, end_slot)
                .await?;

            for block in blocks {
                let block_slot: u64 = block.header.slot.into();

                for tx in &block.transactions {
                    for op in &tx.mantle_tx.ops {
                        if let Op::ChannelInscribe(inscribe) = op
                            && inscribe.channel_id == self.channel_id
                            && let Some(done) =
                                scan.push_msg(block_slot, inscribe.id(), &inscribe.inscription)
                        {
                            return Ok(done);
                        }
                    }
                }
            }

            current_slot = end_slot + 1;
        }

        // Caught up to LIB.
        Ok(PollResult {
            messages: scan.out,
            cursor: Cursor {
                slot: lib_slot,
                last_id: None,
            },
        })
    }
}

/// Internal state machine for cursor-based message scanning.
struct ScanState {
    cursor_slot: u64,
    skip_until: Option<MsgId>,
    out: Vec<ZoneBlock>,
    limit: usize,
}

impl ScanState {
    const fn new(cursor: Cursor, limit: usize) -> Self {
        Self {
            cursor_slot: cursor.slot,
            skip_until: cursor.last_id,
            out: Vec::new(),
            limit,
        }
    }

    /// Process a message. Returns `Some(PollResult)` if limit reached.
    fn push_msg(&mut self, block_slot: u64, msg_id: MsgId, data: &[u8]) -> Option<PollResult> {
        // Once we move past cursor slot, skipping is irrelevant.
        debug_assert!(
            block_slot >= self.cursor_slot,
            "blocks must be scanned forward"
        );
        if block_slot > self.cursor_slot {
            self.skip_until = None;
        }

        // Skip up to (and including) last_id.
        if let Some(skip_id) = self.skip_until {
            if msg_id == skip_id {
                self.skip_until = None;
            }
            return None;
        }

        self.out.push(ZoneBlock {
            id: msg_id,
            data: data.to_vec(),
        });

        if self.out.len() >= self.limit {
            return Some(PollResult {
                messages: std::mem::take(&mut self.out),
                cursor: Cursor {
                    slot: block_slot,
                    last_id: Some(msg_id),
                },
            });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg_id(n: u8) -> MsgId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        MsgId::from(bytes)
    }

    #[test]
    fn resume_within_slot_skip() {
        // Cursor says: resume in slot 0 after msg_id(2)
        let cursor = Cursor {
            slot: 0,
            last_id: Some(msg_id(2)),
        };
        let mut scan = ScanState::new(cursor, 10);

        // Feed messages from slot 0
        assert!(scan.push_msg(0, msg_id(1), &[1]).is_none()); // skipped
        assert!(scan.push_msg(0, msg_id(2), &[2]).is_none()); // skipped (last_id itself)
        assert!(scan.push_msg(0, msg_id(3), &[3]).is_none()); // collected

        assert_eq!(scan.out.len(), 1);
        assert_eq!(scan.out[0].id, msg_id(3));
    }

    #[test]
    fn resume_next_slot() {
        // Cursor says: slot 0 is done, start at slot 1
        let cursor = Cursor {
            slot: 0,
            last_id: None,
        };
        let mut scan = ScanState::new(cursor, 10);

        // Feed messages from slot 1 (skip_until should be None, nothing to skip)
        assert!(scan.push_msg(1, msg_id(1), &[1]).is_none());
        assert!(scan.push_msg(1, msg_id(2), &[2]).is_none());

        assert_eq!(scan.out.len(), 2);
    }

    #[test]
    fn limit_hit_mid_block() {
        let cursor = Cursor::default();
        let mut scan = ScanState::new(cursor, 2);

        // Feed 3 messages, limit is 2
        assert!(scan.push_msg(0, msg_id(1), &[1]).is_none());
        let result = scan.push_msg(0, msg_id(2), &[2]);

        assert!(result.is_some());
        let poll = result.unwrap();
        assert_eq!(poll.messages.len(), 2);
        assert_eq!(poll.cursor.slot, 0);
        assert_eq!(poll.cursor.last_id, Some(msg_id(2)));
    }

    #[test]
    fn skip_clears_when_leaving_cursor_slot() {
        // Cursor points to non-existent msg in slot 0
        let cursor = Cursor {
            slot: 0,
            last_id: Some(msg_id(99)),
        };
        let mut scan = ScanState::new(cursor, 10);

        // All messages in slot 0 are skipped (looking for msg_id(99))
        assert!(scan.push_msg(0, msg_id(1), &[1]).is_none());
        assert!(scan.push_msg(0, msg_id(2), &[2]).is_none());
        assert_eq!(scan.out.len(), 0);

        // Moving to slot 1 clears skip_until
        assert!(scan.push_msg(1, msg_id(3), &[3]).is_none());
        assert_eq!(scan.out.len(), 1);
        assert_eq!(scan.out[0].id, msg_id(3));
    }
}
