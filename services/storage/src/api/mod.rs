pub mod requests;

use std::{
    collections::BTreeMap,
    fmt::{Debug, Display},
    marker::PhantomData,
    num::NonZeroUsize,
    ops::RangeInclusive,
};

use bytes::Bytes;
use futures::{StreamExt as _, future::join_all, stream::BoxStream};
use lb_binary_codec::{
    bincode::{DeserializeOp as _, SerializeOp as _},
    canonical::{BinaryDecode, BinaryEncode},
};
use lb_core::{
    block::{Block, v1},
    events::Events,
    header::HeaderId,
    mantle::{
        TxHash,
        traits::{Hashable, StorageSize},
    },
};
use lb_cryptarchia_engine::{Slot, era::EraVersion};
use overwatch::{
    DynError,
    overwatch::{OverwatchHandle, errors::Error as OverwatchError},
    services::{AsServiceId, relay::OutboundRelay},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;

use crate::{StorageMsg, StorageService};

/// Typed API for the storage service.
pub struct StorageApi<Tx = ()> {
    relay: OutboundRelay<StorageMsg>,
    transaction: PhantomData<fn() -> Tx>,
}

impl<Tx> Clone for StorageApi<Tx> {
    fn clone(&self) -> Self {
        Self {
            relay: self.relay.clone(),
            transaction: PhantomData,
        }
    }
}

impl<Tx> StorageApi<Tx> {
    #[must_use]
    pub const fn new(relay: OutboundRelay<StorageMsg>) -> Self {
        Self {
            relay,
            transaction: PhantomData,
        }
    }

    /// Connect to storage through the Overwatch handle. Use [`Self::new`] when
    /// a relay is already available.
    pub async fn from_overwatch_handle<RuntimeServiceId>(
        handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, OverwatchError>
    where
        RuntimeServiceId: AsServiceId<StorageService<RuntimeServiceId>> + Debug + Display + Sync,
    {
        handle
            .relay::<StorageService<RuntimeServiceId>>()
            .await
            .map(Self::new)
    }

    pub async fn store<Value: Serialize>(&self, key: Bytes, value: Value) -> Result<(), DynError> {
        self.store_raw(key, value.to_bytes()?).await
    }

    /// Stores `value` as it is.
    pub async fn store_raw(&self, key: Bytes, value: Bytes) -> Result<(), DynError> {
        self.relay.send(StorageMsg::Store { key, value }).await?;
        Ok(())
    }

    pub async fn load<Value: DeserializeOwned>(
        &self,
        key: Bytes,
    ) -> Result<Option<Value>, DynError> {
        let bytes = self
            .request(|reply_channel| StorageMsg::Load { key, reply_channel })
            .await?;
        Ok(bytes.map(|bytes| Value::from_bytes(&bytes).expect("Failed to decode stored value")))
    }

    async fn request<Reply>(
        &self,
        message: impl FnOnce(oneshot::Sender<Reply>) -> StorageMsg,
    ) -> Result<Reply, DynError> {
        let (sender, receiver) = oneshot::channel();
        self.relay.send(message(sender)).await?;
        Ok(receiver.await?)
    }

    /// The canonical encoding of a stored block, as peers sync it.
    pub async fn get_block_bytes(&self, id: &HeaderId) -> Result<Option<Bytes>, DynError> {
        self.request(|response_tx| StorageMsg::GetBlock {
            header_id: *id,
            response_tx,
        })
        .await?
        .map(|record| split_stored_block(&record).map(|(_, block)| block))
        .transpose()
    }

    pub async fn get_block_parent(&self, id: &HeaderId) -> Option<HeaderId> {
        self.optional_request(|response_tx| StorageMsg::GetBlockParent {
            header_id: *id,
            response_tx,
        })
        .await
    }

    pub async fn get_block_events(&self, id: &HeaderId) -> Option<Events> {
        let bytes = self
            .optional_request(|response_tx| StorageMsg::GetBlockEvents {
                header_id: *id,
                response_tx,
            })
            .await?;
        Events::try_from(bytes)
            .inspect_err(|error| {
                tracing::error!(%error, "Failed to convert block events loaded from storage");
            })
            .ok()
    }

    async fn optional_request<Reply>(
        &self,
        message: impl FnOnce(oneshot::Sender<Option<Reply>>) -> StorageMsg,
    ) -> Option<Reply> {
        let (sender, receiver) = oneshot::channel();
        self.relay.send(message(sender)).await.unwrap();
        receiver.await.unwrap_or_else(|error| {
            tracing::error!(%error, "Failed to receive response from storage relay");
            None
        })
    }

    pub async fn get_immutable_block_id(&self, slot: Slot) -> Result<Option<HeaderId>, DynError> {
        self.request(|response_tx| StorageMsg::GetImmutableBlockId { slot, response_tx })
            .await
    }

    pub async fn store_immutable_block_ids(
        &self,
        ids: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), DynError> {
        self.request(|response_tx| StorageMsg::StoreImmutableBlockIds { ids, response_tx })
            .await?
            .map_err(Into::into)
    }

    pub async fn scan_immutable_block_ids(
        &self,
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
        descending: bool,
    ) -> Result<Vec<HeaderId>, DynError> {
        self.request(|response_tx| {
            if descending {
                StorageMsg::ScanImmutableBlockIdsReverse {
                    slot_range,
                    limit,
                    response_tx,
                }
            } else {
                StorageMsg::ScanImmutableBlockIds {
                    slot_range,
                    limit,
                    response_tx,
                }
            }
        })
        .await
    }

    pub async fn remove_transactions(&self, hashes: &[TxHash]) -> Result<(), DynError>
    where
        Tx: Hashable<Hash: Into<TxHash>>,
    {
        self.relay
            .send(StorageMsg::RemoveTransactions {
                tx_hashes: hashes.to_vec(),
            })
            .await?;
        Ok(())
    }
}

impl<Tx: Serialize + Hashable<Hash: Into<TxHash>>> StorageApi<Tx> {
    /// Store an item under its externally supplied transaction hash.
    pub async fn store_transaction(&self, hash: TxHash, transaction: Tx) -> Result<(), DynError> {
        let transactions = [(hash, transaction.to_bytes()?)].into();
        self.relay
            .send(StorageMsg::StoreTransactions { transactions })
            .await?;
        Ok(())
    }
}

impl<Tx> StorageApi<Tx>
where
    Tx: BinaryEncode + Hashable<Hash = TxHash> + StorageSize,
{
    /// Stores `block`, of an era of `version`, with its events and the
    /// immutable block identifiers it makes final.
    pub async fn store_block_data(
        &self,
        id: HeaderId,
        parent_id: HeaderId,
        block: Block<Tx>,
        version: EraVersion,
        events: Events,
        immutable_ids: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), DynError> {
        let block = stored_block(version, &block);
        let events = Bytes::try_from(events)?;
        self.request(|response_tx| StorageMsg::StoreBlockData {
            header_id: id,
            parent_id,
            block,
            events,
            immutable_ids,
            response_tx,
        })
        .await?
        .map_err(Into::into)
    }
}

impl<Tx> StorageApi<Tx>
where
    Tx: BinaryDecode<Context = ()> + Hashable<Hash = TxHash> + StorageSize,
{
    pub async fn get_block(&self, id: &HeaderId) -> Option<Block<Tx>> {
        self.try_get_block(id).await.ok().flatten()
    }

    /// Read and verify a block, returning storage and decoding errors.
    pub async fn try_get_block(&self, id: &HeaderId) -> Result<Option<Block<Tx>>, DynError> {
        self.request(|response_tx| StorageMsg::GetBlock {
            header_id: *id,
            response_tx,
        })
        .await?
        .map(|record| decode_stored_block(&record))
        .transpose()
    }

    /// Read and verify a block, returning storage and decoding errors.
    pub async fn load_block(&self, id: &HeaderId) -> Result<Option<Block<Tx>>, DynError> {
        self.try_get_block(id).await
    }

    pub async fn remove_block(&self, id: HeaderId) -> Result<Option<Block<Tx>>, DynError> {
        self.request(|response_tx| StorageMsg::RemoveBlock {
            header_id: id,
            response_tx,
        })
        .await?
        .map(|record| decode_stored_block(&record))
        .transpose()
    }

    pub async fn remove_blocks(
        &self,
        ids: impl Iterator<Item = HeaderId>,
    ) -> impl Iterator<Item = Result<Option<Block<Tx>>, DynError>> {
        join_all(ids.map(|id| self.remove_block(id)))
            .await
            .into_iter()
    }
}

/// How a block is stored: the version of the era of its slot, a `u16` in
/// little-endian order, then the block's canonical encoding. A node reads its
/// own blocks with the version stored next to them, without the era schedule,
/// and serves peers the encoding alone.
fn stored_block<Tx>(version: EraVersion, block: &Block<Tx>) -> Bytes
where
    Tx: BinaryEncode + Hashable<Hash = TxHash> + StorageSize,
{
    let mut record = Vec::with_capacity(size_of::<u16>() + block.encoded_length());
    record.extend_from_slice(&version.tag().to_le_bytes());
    block.encode_into(&mut record);
    record.into()
}

/// The version and the canonical encoding of a block stored as
/// [`stored_block`] lays it out.
fn split_stored_block(record: &Bytes) -> Result<(EraVersion, Bytes), DynError> {
    let tag = record
        .first_chunk()
        .ok_or("a stored block starts with the version of its era")?;
    let version = EraVersion::try_from(u16::from_le_bytes(*tag))?;
    Ok((version, record.slice(size_of::<u16>()..)))
}

fn decode_stored_block<Tx>(record: &Bytes) -> Result<Block<Tx>, DynError>
where
    Tx: BinaryDecode<Context = ()> + Hashable<Hash = TxHash> + StorageSize,
{
    let (version, block) = split_stored_block(record)?;
    match version {
        EraVersion::V1 => Ok(Block::V1(v1::Block::decode_all(&block, &())?)),
    }
}

impl<Tx: Serialize + Hashable<Hash = TxHash>> StorageApi<Tx> {
    pub async fn store_transactions(&self, transactions: Vec<Tx>) -> Result<(), DynError> {
        let transactions = transactions
            .into_iter()
            .map(|tx| tx.to_bytes().map(|bytes| (tx.hash(), bytes)))
            .collect::<Result<_, _>>()?;
        self.relay
            .send(StorageMsg::StoreTransactions { transactions })
            .await?;
        Ok(())
    }
}

impl<Tx: DeserializeOwned + Hashable<Hash: Into<TxHash>> + Send + 'static> StorageApi<Tx> {
    pub async fn get_transactions(
        &self,
        hashes: Vec<TxHash>,
    ) -> Result<BoxStream<'static, Tx>, DynError> {
        Ok(self
            .try_get_transactions(hashes)
            .await?
            .filter_map(async |result| result.ok())
            .boxed())
    }

    /// Like [`Self::get_transactions`], but yields decode errors instead of
    /// skipping them.
    pub async fn try_get_transactions(
        &self,
        hashes: Vec<TxHash>,
    ) -> Result<BoxStream<'static, Result<Tx, DynError>>, DynError> {
        let stream = self
            .request(|response_tx| StorageMsg::GetTransactions {
                tx_hashes: hashes,
                response_tx,
            })
            .await?;
        Ok(stream
            .map(|bytes| Tx::from_bytes(&bytes).map_err(Into::into))
            .boxed())
    }
}
