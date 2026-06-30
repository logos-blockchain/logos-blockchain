use futures::{Stream, StreamExt as _};
use lb_common_http_client::Slot;
use lb_core::mantle::ops::channel::ChannelId;
use lb_log_targets::zone_sdk;
use tracing::warn;

use crate::{ZoneMessage, adapter};

const TARGET: &str = zone_sdk::INDEXER;

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
                        warn!(target: TARGET, "Failed to fetch LIB block {header_id}: {e}");
                        // TODO: return error to stream, and stop stream
                        return None;
                    }
                };

                Some(stream)
            }
        });

        Ok(stream.flatten())
    }

    /// Stream finalized [`ZoneMessage`]s from `last_slot` (exclusive) up to
    /// LIB.
    ///
    /// `last_slot` is the last slot the caller has fully consumed. `None`
    /// means cold start — streaming begins from genesis. The caller is
    /// responsible for persisting `last_slot` only after the messages of that
    /// slot are durably processed; on crash before persist, restart with the
    /// previous cursor and re-process. Deposits/withdraws carry no `MsgId`,
    /// so this is the only safe resume point — a finer-grained cursor would
    /// either skip them or replay them inconsistently across restarts.
    pub async fn next_messages(
        &self,
        last_slot: Option<Slot>,
    ) -> Result<impl Stream<Item = (ZoneMessage, Slot)> + '_, Error> {
        let lib_slot = self.node.consensus_info().await?.cryptarchia_info.lib_slot;
        let start_slot = last_slot.map_or_else(Slot::genesis, |s| s.strict_add(1.into()));

        #[expect(
            closure_returning_async_block,
            reason = "Signature expected by `unfold`"
        )]
        let stream = futures::stream::unfold(start_slot, move |current_slot| async move {
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
                Ok(messages) => Some((messages, end_slot.strict_add(1.into()))),
                Err(e) => {
                    warn!(target: TARGET,
                        ?current_slot, ?end_slot, err = ?e,
                        "Failed to fetch zone messages from blocks",
                    );
                    // TODO: return error to stream
                    None
                }
            }
        })
        .flatten();

        Ok(stream)
    }
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
        mantle::{
            NoteId, SignedMantleTx,
            ledger::Inputs,
            ops::channel::{MsgId, deposit::Metadata, inscribe::Inscription},
        },
    };
    use lb_groth16::Fr;
    use lb_http_api_common::queries::BlocksStreamQuery;

    use super::*;
    use crate::{Deposit, ZoneBlock, adapter::BoxStream};

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
                deposit_msg(Inputs::new([NoteId::from(Fr::from(10u32))]), 0, [10].into()),
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
                deposit_msg(Inputs::new([NoteId::from(Fr::from(10u32))]), 0, [10].into()),
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
    async fn next_messages_resume_from_cursor() {
        let messages = vec![
            (block_msg(1, &[1]), Slot::new(0)),
            (
                deposit_msg(Inputs::new([NoteId::from(Fr::from(10u32))]), 0, [10].into()),
                Slot::new(0),
            ),
            (block_msg(2, &[2]), Slot::new(1)),
            (
                deposit_msg(Inputs::new([NoteId::from(Fr::from(11u32))]), 0, [11].into()),
                Slot::new(2),
            ),
            (block_msg(3, &[3]), Slot::new(2)),
        ];
        let indexer = indexer(Slot::new(2), messages.clone());

        // Last fully consumed slot is 1; resume from slot 2.
        let stream = indexer.next_messages(Some(Slot::new(1))).await.unwrap();
        futures::pin_mut!(stream);
        assert_eq!(stream.next().await.as_ref(), Some(&messages[3]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[4]));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_messages_cursor_at_lib_emits_nothing() {
        let messages = vec![
            (block_msg(1, &[1]), Slot::new(0)),
            (
                deposit_msg(Inputs::new([NoteId::from(Fr::from(10u32))]), 0, [10].into()),
                Slot::new(0),
            ),
            (block_msg(2, &[2]), Slot::new(1)),
        ];
        let indexer = indexer(Slot::new(1), messages);

        // Cursor at LIB — nothing new to emit.
        let stream = indexer.next_messages(Some(Slot::new(1))).await.unwrap();
        futures::pin_mut!(stream);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_messages_cold_start_includes_genesis() {
        // Inscription at slot 0 (genesis) must be emitted on cold start
        // (cursor None).
        let messages = vec![
            (block_msg(1, &[1]), Slot::genesis()),
            (block_msg(2, &[2]), Slot::new(1)),
        ];
        let indexer = indexer(Slot::new(1), messages.clone());

        let stream = indexer.next_messages(None).await.unwrap();
        futures::pin_mut!(stream);
        assert_eq!(stream.next().await.as_ref(), Some(&messages[0]));
        assert_eq!(stream.next().await.as_ref(), Some(&messages[1]));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_messages_across_batches() {
        let messages = vec![
            (block_msg(1, &[1]), Slot::new(0)),
            (
                deposit_msg(Inputs::new([NoteId::from(Fr::from(10u32))]), 0, [10].into()),
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
                deposit_msg(Inputs::new([NoteId::from(Fr::from(11u32))]), 0, [11].into()),
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

        // Cursor at slot 200 — resume from slot 201, which spans multiple
        // batches up to LIB at slot 400.
        let stream = indexer
            .next_messages(Some(BATCH_SIZE.into_inner().checked_mul(2).unwrap().into()))
            .await
            .unwrap();
        futures::pin_mut!(stream);
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
            data: Inscription::try_from(data).unwrap(),
        })
    }

    fn deposit_msg(inputs: Inputs, amount: u64, metadata: Metadata) -> ZoneMessage {
        ZoneMessage::Deposit(Deposit {
            inputs,
            amount,
            metadata,
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

        async fn time_info(
            &self,
        ) -> Result<lb_common_http_client::TimeInfo, lb_common_http_client::Error> {
            Ok(lb_common_http_client::TimeInfo {
                slot_duration_ms: 1_000,
                genesis_time_unix_ms: 0,
                current_slot: 0,
                current_epoch: 0,
            })
        }

        async fn channel_state(
            &self,
            _channel_id: ChannelId,
        ) -> Result<Option<lb_core::mantle::channel::ChannelState>, lb_common_http_client::Error>
        {
            Ok(None)
        }

        async fn block_stream(
            &self,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn blocks_range_stream(
            &self,
            _params: BlocksStreamQuery,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn block(
            &self,
            _id: HeaderId,
        ) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
            Ok(None)
        }

        async fn block_events(
            &self,
            _id: HeaderId,
        ) -> Result<Option<lb_common_http_client::Events>, lb_common_http_client::Error> {
            Ok(None)
        }

        async fn immutable_blocks(
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
        ) -> Result<BoxStream<ZoneMessage>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn zone_messages_in_blocks(
            &self,
            slot_from: Slot,
            slot_to: Slot,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<(ZoneMessage, Slot)>, lb_common_http_client::Error> {
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
