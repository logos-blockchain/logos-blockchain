use futures::{Stream, StreamExt as _, future};
use lb_common_http_client::Slot;
use lb_core::mantle::ops::channel::{ChannelId, MsgId};
use tracing::warn;

use crate::{ZoneMessage, adapter};

/// Indexer errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] lb_common_http_client::Error),
}

/// Zone indexer — reads finalized zone messages from a channel.
pub struct ZoneIndexer<Node> {
    channel_id: ChannelId,
    node: Node,
}

const BATCH_SIZE: Slot = Slot::new(100);

impl<Node> ZoneIndexer<Node>
where
    Node: adapter::Node + Clone + Sync,
{
    #[must_use]
    pub const fn new(channel_id: ChannelId, node: Node) -> Self {
        Self { channel_id, node }
    }

    /// Subscribe to live [`ZoneMessage`]s as they finalize.
    pub async fn follow(&self) -> Result<impl Stream<Item = ZoneMessage> + '_, Error> {
        let lib_stream = self.node.lib_stream().await?;

        let channel_id = self.channel_id;
        let stream = lib_stream.filter_map(move |block_info| {
            let header_id = block_info.header_id;

            async move {
                let stream = match self
                    .node
                    .zone_messages_in_block(header_id, channel_id)
                    .await
                {
                    Ok(stream) => stream,
                    Err(e) => {
                        warn!("Failed to fetch LIB block {header_id}: {e}");
                        // TODO: return error to stream, and stop stream
                        return None;
                    }
                };

                Some(stream)
            }
        });

        Ok(stream.flatten())
    }

    /// Stream finalized [`ZoneMessage`]s from `last_zone_block` (excluded)
    /// up to LIB.
    pub async fn next_messages(
        &self,
        last_zone_block: Option<(MsgId, Slot)>,
    ) -> Result<impl Stream<Item = (ZoneMessage, Slot)> + '_, Error> {
        let lib_slot = self.node.consensus_info().await?.cryptarchia_info.lib_slot;
        let current_slot = last_zone_block
            .as_ref()
            .map_or_else(Slot::genesis, |(_, slot)| *slot);
        let mut skip_until = last_zone_block;

        #[expect(
            closure_returning_async_block,
            reason = "Signature expected by `unfold`"
        )]
        let stream = futures::stream::unfold(current_slot, move |current_slot| async move {
            if current_slot > lib_slot {
                return None;
            }

            let end_slot = (Slot::from(
                current_slot
                    .into_inner()
                    .saturating_add(BATCH_SIZE.into_inner())
                    .checked_sub(1)
                    .expect("slot shouldn't overflow"),
            ))
            .min(lib_slot);

            match self
                .node
                .zone_messages_in_blocks(current_slot, end_slot, self.channel_id)
                .await
            {
                Ok(messages) => Some((messages, end_slot + 1)),
                Err(e) => {
                    warn!(
                        ?current_slot, ?end_slot, err = ?e,
                        "Failed to fetch zone messages from blocks",
                    );
                    // TODO: return error to stream
                    None
                }
            }
        })
        .flatten()
        .skip_while(move |(message, slot)| {
            future::ready(should_skip(message, *slot, &mut skip_until))
        });

        Ok(stream)
    }
}

/// Returns `true` if the message should be skipped.
///
/// `skip_until` is set to `None` once there is no need to skip anymore.
/// (e.g., once the cursor message is found, or cursor slot has already passed)
fn should_skip(message: &ZoneMessage, slot: Slot, skip_until: &mut Option<(MsgId, Slot)>) -> bool {
    let Some((cursor_msg_id, cursor_msg_slot)) = *skip_until else {
        return false;
    };

    // Passed the cursor slot — stop skipping.
    if slot > cursor_msg_slot {
        *skip_until = None;
        return false;
    }

    match message {
        ZoneMessage::Block(block) => {
            if block.id == cursor_msg_id {
                // Found the cursor message — stop skipping after this.
                *skip_until = None;
            }
        }
        // Deposits/withdraws have no ID, so keep skipping.
        ZoneMessage::Deposit(_) | ZoneMessage::Withdraw(_) => {}
    }
    true
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use lb_common_http_client::{
        ApiBlock, BlockInfo, ChainServiceInfo, ChainServiceMode, CryptarchiaInfo,
        ProcessedBlockEvent, State,
    };
    use lb_core::{
        header::HeaderId,
        mantle::{NoteId, SignedMantleTx, ledger::Inputs},
    };
    use lb_groth16::Fr;

    use super::*;
    use crate::{Deposit, ZoneBlock};

    #[tokio::test]
    async fn next_messages_empty() {
        let indexer = indexer(Slot::new(1), Vec::new());

        let stream = indexer.next_messages(None).await.unwrap();
        futures::pin_mut!(stream);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_messages_no_skip() {
        let messages = vec![
            (block_msg(1, &[1]), Slot::new(0)),
            (
                deposit_msg(Inputs::new(vec![NoteId::from(Fr::from(10u32))]), &[10]),
                Slot::new(0),
            ),
            (block_msg(2, &[2]), Slot::new(1)),
        ];
        let indexer = indexer(Slot::new(1), messages.clone());

        let stream = indexer.next_messages(None).await.unwrap();
        futures::pin_mut!(stream);
        assert_eq!(stream.next().await.as_ref(), Some(&messages[0]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[1]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[2]));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_messages_until_lib() {
        let messages = vec![
            (block_msg(1, &[1]), Slot::new(0)),
            (
                deposit_msg(Inputs::new(vec![NoteId::from(Fr::from(10u32))]), &[10]),
                Slot::new(1),
            ),
            (block_msg(2, &[2]), Slot::new(2)), // after LIB
        ];
        let indexer = indexer(Slot::new(1), messages.clone());

        let stream = indexer.next_messages(None).await.unwrap();
        futures::pin_mut!(stream);
        assert_eq!(stream.next().await.as_ref(), Some(&messages[0]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[1]));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_messages_skip() {
        let messages = vec![
            (block_msg(1, &[1]), Slot::new(0)),
            (
                deposit_msg(Inputs::new(vec![NoteId::from(Fr::from(10u32))]), &[10]),
                Slot::new(0),
            ),
            (block_msg(2, &[2]), Slot::new(1)),
            (
                deposit_msg(Inputs::new(vec![NoteId::from(Fr::from(11u32))]), &[11]),
                Slot::new(2),
            ),
            (block_msg(3, &[3]), Slot::new(2)),
        ];
        let indexer = indexer(Slot::new(2), messages.clone());

        // Skip until msg_id(2) in slot 1
        let stream = indexer
            .next_messages(Some((msg_id(2), 1.into())))
            .await
            .unwrap();
        futures::pin_mut!(stream);
        assert_eq!(stream.next().await.as_ref(), Some(&messages[3]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[4]));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_messages_skip_msg_not_found() {
        let messages = vec![
            (block_msg(1, &[1]), Slot::new(0)),
            (
                deposit_msg(Inputs::new(vec![NoteId::from(Fr::from(10u32))]), &[10]),
                Slot::new(0),
            ),
            (block_msg(2, &[2]), Slot::new(1)),
            (
                deposit_msg(Inputs::new(vec![NoteId::from(Fr::from(11u32))]), &[11]),
                Slot::new(2),
            ),
            (block_msg(4, &[4]), Slot::new(2)),
        ];
        let indexer = indexer(Slot::new(2), messages.clone());

        // Skip until msg_id(3) in slot 1, but it doesn't exist.
        // Then, all msgs after slot 1 must be returned.
        let stream = indexer
            .next_messages(Some((msg_id(3), 1.into())))
            .await
            .unwrap();
        futures::pin_mut!(stream);
        assert_eq!(stream.next().await.as_ref(), Some(&messages[3]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[4]));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_messages_skip_but_nothing_left() {
        let messages = vec![
            (block_msg(1, &[1]), Slot::new(0)),
            (
                deposit_msg(Inputs::new(vec![NoteId::from(Fr::from(10u32))]), &[10]),
                Slot::new(0),
            ),
            (block_msg(2, &[2]), Slot::new(1)),
        ];
        let indexer = indexer(Slot::new(2), messages.clone());

        // Skip until msg_id(3) in slot 1, but it doesn't exist.
        // Then, all msgs after slot 1 must be returned.
        let stream = indexer
            .next_messages(Some((msg_id(3), 1.into())))
            .await
            .unwrap();
        futures::pin_mut!(stream);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_messages_across_batches() {
        let messages = vec![
            (block_msg(1, &[1]), Slot::new(0)),
            (
                deposit_msg(Inputs::new(vec![NoteId::from(Fr::from(10u32))]), &[10]),
                BATCH_SIZE,
            ),
            (
                block_msg(2, &[2]),
                BATCH_SIZE.into_inner().checked_mul(2).unwrap().into(),
            ),
            (
                block_msg(3, &[3]),
                BATCH_SIZE.into_inner().checked_mul(2).unwrap().into(),
            ),
            (
                deposit_msg(Inputs::new(vec![NoteId::from(Fr::from(11u32))]), &[11]),
                BATCH_SIZE.into_inner().checked_mul(3).unwrap().into(),
            ),
            (
                block_msg(4, &[4]),
                BATCH_SIZE.into_inner().checked_mul(3).unwrap().into(),
            ),
            (
                block_msg(5, &[5]),
                BATCH_SIZE.into_inner().checked_mul(4).unwrap().into(),
            ),
        ];
        let indexer = indexer(
            BATCH_SIZE.into_inner().checked_mul(4).unwrap().into(),
            messages.clone(),
        );

        let stream = indexer
            .next_messages(Some((
                msg_id(2),
                BATCH_SIZE.into_inner().checked_mul(2).unwrap().into(),
            )))
            .await
            .unwrap();
        futures::pin_mut!(stream);
        assert_eq!(stream.next().await.as_ref(), Some(&messages[3]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[4]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[5]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[6]));
        assert!(stream.next().await.is_none());
    }

    fn msg_id(n: u8) -> MsgId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        MsgId::from(bytes)
    }

    fn block_msg(id: u8, data: &[u8]) -> ZoneMessage {
        ZoneMessage::Block(ZoneBlock {
            id: msg_id(id),
            data: data.to_vec(),
        })
    }

    fn deposit_msg(inputs: Inputs, metadata: &[u8]) -> ZoneMessage {
        ZoneMessage::Deposit(Deposit {
            inputs,
            metadata: metadata.to_vec(),
        })
    }

    fn indexer(lib_slot: Slot, messages: Vec<(ZoneMessage, Slot)>) -> ZoneIndexer<MockNode> {
        let node = MockNode { lib_slot, messages };
        ZoneIndexer::new(ChannelId::from([0u8; 32]), node)
    }

    /// Mock node that returns preconfigured zone messages.
    #[derive(Clone)]
    struct MockNode {
        lib_slot: Slot,
        messages: Vec<(ZoneMessage, Slot)>,
    }

    #[async_trait]
    impl adapter::Node for MockNode {
        async fn consensus_info(&self) -> Result<ChainServiceInfo, lb_common_http_client::Error> {
            Ok(ChainServiceInfo {
                cryptarchia_info: CryptarchiaInfo {
                    lib: HeaderId::from([0; 32]),
                    lib_slot: self.lib_slot,
                    tip: HeaderId::from([0; 32]),
                    slot: self.lib_slot,
                    height: 0,
                },
                mode: ChainServiceMode::Started(State::Online),
            })
        }

        async fn block_stream(
            &self,
        ) -> Result<adapter::BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn lib_stream(
            &self,
        ) -> Result<adapter::BoxStream<BlockInfo>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn block(
            &self,
            _id: HeaderId,
        ) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
            Ok(None)
        }

        async fn blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
        ) -> Result<Vec<ApiBlock>, lb_common_http_client::Error> {
            Ok(Vec::new())
        }

        async fn zone_messages_in_block(
            &self,
            _id: HeaderId,
            _channel_id: ChannelId,
        ) -> Result<adapter::BoxStream<ZoneMessage>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn zone_messages_in_blocks(
            &self,
            slot_from: Slot,
            slot_to: Slot,
            _channel_id: ChannelId,
        ) -> Result<adapter::BoxStream<(ZoneMessage, Slot)>, lb_common_http_client::Error> {
            let msgs: Vec<_> = self
                .messages
                .iter()
                .filter(move |(_, slot)| *slot >= slot_from && *slot <= slot_to)
                .cloned()
                .collect();
            Ok(Box::pin(futures::stream::iter(msgs)))
        }

        async fn post_transaction(
            &self,
            _tx: SignedMantleTx,
        ) -> Result<(), lb_common_http_client::Error> {
            unimplemented!()
        }
    }
}
