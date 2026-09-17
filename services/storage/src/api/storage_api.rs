use std::{collections::BTreeMap, marker::PhantomData, num::NonZeroUsize, ops::RangeInclusive};

use bytes::Bytes;
use futures::{StreamExt as _, future::join_all, stream::BoxStream};
use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
use lb_core::{
    block::Block,
    events::Events,
    header::HeaderId,
    mantle::{
        TxHash,
        traits::{Hashable, StorageSize},
    },
};
use lb_cryptarchia_engine::Slot;
use overwatch::{DynError, services::relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;

use super::chain::requests::ChainApiRequest;
use crate::StorageMsg;

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

    pub async fn store<Value: Serialize>(&self, key: Bytes, value: Value) -> Result<(), DynError> {
        let value = value.to_bytes()?;
        self.relay.send(StorageMsg::Store { key, value }).await?;
        Ok(())
    }

    pub async fn load<Value: Serialize + DeserializeOwned>(
        &self,
        key: Bytes,
    ) -> Result<Option<Value>, DynError> {
        let (message, receiver) = StorageMsg::new_load_message(key);
        self.relay.send(message).await?;
        Ok(receiver.recv().await?)
    }

    async fn request<Reply>(
        &self,
        message: impl FnOnce(oneshot::Sender<Reply>) -> StorageMsg,
    ) -> Result<Reply, DynError> {
        let (sender, receiver) = oneshot::channel();
        self.relay.send(message(sender)).await?;
        Ok(receiver.await?)
    }

    /// Return the stored block bytes.
    pub async fn get_block_bytes(&self, id: &HeaderId) -> Result<Option<Bytes>, DynError> {
        self.request(|sender| StorageMsg::get_block_request(*id, sender))
            .await
    }

    pub async fn get_block_parent(&self, id: &HeaderId) -> Option<HeaderId> {
        self.optional_request(|sender| StorageMsg::get_block_parent_request(*id, sender))
            .await
    }

    pub async fn get_block_events(&self, id: &HeaderId) -> Option<Events> {
        let bytes = self
            .optional_request(|sender| StorageMsg::get_block_events_request(*id, sender))
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
        self.request(|sender| StorageMsg::get_immutable_block_id_request(slot, sender))
            .await
    }

    pub async fn store_immutable_block_ids(
        &self,
        ids: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), DynError> {
        self.request(|sender| StorageMsg::store_immutable_block_ids_request(ids, sender))
            .await?
            .map_err(Into::into)
    }

    pub async fn scan_immutable_block_ids(
        &self,
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
        descending: bool,
    ) -> Result<Vec<HeaderId>, DynError> {
        self.request(|response_tx| StorageMsg::Api {
            request: if descending {
                ChainApiRequest::ScanImmutableBlockIdsReverse {
                    slot_range,
                    limit,
                    response_tx,
                }
            } else {
                ChainApiRequest::ScanImmutableBlockIds {
                    slot_range,
                    limit,
                    response_tx,
                }
            },
        })
        .await
    }

    pub async fn remove_transactions(&self, hashes: &[TxHash]) -> Result<(), DynError> {
        self.relay
            .send(StorageMsg::remove_transactions_request(hashes.to_vec()))
            .await?;
        Ok(())
    }
}

impl<Tx: Serialize> StorageApi<Tx> {
    /// Store an item under its externally supplied transaction hash.
    pub async fn store_transaction(&self, hash: TxHash, transaction: Tx) -> Result<(), DynError> {
        let transactions = [(hash, transaction.to_bytes()?)].into();
        self.relay
            .send(StorageMsg::store_transactions_request(transactions))
            .await?;
        Ok(())
    }
}

impl<Tx> StorageApi<Tx>
where
    Tx: Clone + Eq + Serialize + DeserializeOwned + Hashable<Hash = TxHash> + StorageSize,
{
    pub async fn get_block(&self, id: &HeaderId) -> Option<Block<Tx>> {
        let bytes = self
            .optional_request(|sender| StorageMsg::get_block_request(*id, sender))
            .await?;
        Block::try_from(bytes).ok()
    }

    /// Read and verify a block, returning storage and decoding errors.
    pub async fn try_get_block(&self, id: &HeaderId) -> Result<Option<Block<Tx>>, DynError> {
        self.get_block_bytes(id)
            .await?
            .map(Block::try_from)
            .transpose()
            .map_err(Into::into)
    }

    /// Decode a stored block without the additional `into_verified` pass.
    pub async fn load_block(&self, id: &HeaderId) -> Result<Option<Block<Tx>>, DynError> {
        self.load(Bytes::copy_from_slice(&<[u8; 32]>::from(*id)))
            .await
    }

    pub async fn store_block_data(
        &self,
        id: HeaderId,
        parent_id: HeaderId,
        block: Block<Tx>,
        events: Events,
        immutable_ids: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), DynError> {
        let block = Bytes::try_from(block)?;
        let events = Bytes::try_from(events)?;
        self.request(|sender| {
            StorageMsg::store_block_data_request(
                id,
                parent_id,
                block,
                events,
                immutable_ids,
                sender,
            )
        })
        .await?
        .map_err(Into::into)
    }

    pub async fn remove_block(&self, id: HeaderId) -> Result<Option<Block<Tx>>, DynError> {
        let bytes = self
            .request(|sender| StorageMsg::remove_block_request(id, sender))
            .await?;
        bytes.map(Block::try_from).transpose().map_err(Into::into)
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

impl<Tx: Serialize + Hashable<Hash = TxHash>> StorageApi<Tx> {
    pub async fn store_transactions(&self, transactions: Vec<Tx>) -> Result<(), DynError> {
        let transactions = transactions
            .into_iter()
            .map(|tx| tx.to_bytes().map(|bytes| (tx.hash(), bytes)))
            .collect::<Result<_, _>>()?;
        self.relay
            .send(StorageMsg::store_transactions_request(transactions))
            .await?;
        Ok(())
    }
}

impl<Tx: DeserializeOwned + Send + 'static> StorageApi<Tx> {
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
            .request(|sender| StorageMsg::get_transactions_request(hashes, sender))
            .await?;
        Ok(stream
            .map(|bytes| Tx::from_bytes(&bytes).map_err(Into::into))
            .boxed())
    }
}

#[cfg(all(test, feature = "rocksdb-backend"))]
mod tests {
    use futures::stream;
    use lb_core::mantle::{
        SignedOps,
        ledger::verification_mode::StandardMode,
        transactions::states::{Preverified, Unverified},
    };
    use tokio::sync::mpsc;

    use super::*;

    fn api<Tx>() -> (StorageApi<Tx>, mpsc::Receiver<StorageMsg>) {
        let (sender, receiver) = mpsc::channel(4);
        (StorageApi::new(OutboundRelay::new(sender)), receiver)
    }

    #[tokio::test]
    async fn store_load_and_block_events() {
        let (api, mut receiver) = api::<()>();
        let replies = tokio::spawn(async move {
            let Some(StorageMsg::Store { key, value }) = receiver.recv().await else {
                panic!("Expected a store request");
            };
            let Some(StorageMsg::Load {
                key: loaded_key,
                reply_channel,
            }) = receiver.recv().await
            else {
                panic!("Expected a load request");
            };
            assert_eq!(key, loaded_key);
            reply_channel.send(Some(value)).unwrap();
            let Some(StorageMsg::Api {
                request: ChainApiRequest::GetBlockEvents { response_tx, .. },
            }) = receiver.recv().await
            else {
                panic!("Expected an events request");
            };
            response_tx
                .send(Some(Events::new().to_bytes().unwrap()))
                .unwrap();
        });
        let key = Bytes::from_static(b"state");
        api.store(key.clone(), 42u64).await.unwrap();
        assert_eq!(api.load::<u64>(key).await.unwrap(), Some(42));
        assert!(
            api.get_block_events(&HeaderId::from([1; 32]))
                .await
                .unwrap()
                .is_empty()
        );
        replies.await.unwrap();
    }

    #[tokio::test]
    async fn get_block_missing_or_invalid() {
        let (api, mut receiver) = api::<SignedOps<Unverified, StandardMode>>();
        let replies = tokio::spawn(async move {
            for reply in [None, Some(Bytes::new())] {
                let Some(StorageMsg::Api {
                    request: ChainApiRequest::GetBlock { response_tx, .. },
                }) = receiver.recv().await
                else {
                    panic!("Expected a block request");
                };
                response_tx.send(reply).unwrap();
            }
        });
        let id = HeaderId::from([1; 32]);
        assert!(api.get_block(&id).await.is_none());
        assert!(api.get_block(&id).await.is_none());
        replies.await.unwrap();
    }

    #[tokio::test]
    async fn get_preverified_block_invalid_encoding() {
        let (api, mut receiver) = api::<SignedOps<Preverified, StandardMode>>();
        let reply = tokio::spawn(async move {
            let Some(StorageMsg::Api {
                request: ChainApiRequest::GetBlock { response_tx, .. },
            }) = receiver.recv().await
            else {
                panic!("Expected a block request");
            };
            response_tx.send(Some(Bytes::new())).unwrap();
        });
        assert!(api.get_block(&HeaderId::from([1; 32])).await.is_none());
        reply.await.unwrap();
    }

    #[tokio::test]
    async fn get_transactions_skips_invalid_encoding() {
        let (api, mut receiver) = api::<u64>();
        let reply = tokio::spawn(async move {
            let Some(StorageMsg::Api {
                request: ChainApiRequest::GetTransactions { response_tx, .. },
            }) = receiver.recv().await
            else {
                panic!("Expected a transaction request");
            };
            let stream = stream::iter([42u64.to_bytes().unwrap(), Bytes::new()]);
            assert!(response_tx.send(Box::pin(stream)).is_ok());
        });
        let mut transactions = api.get_transactions(vec![TxHash::default()]).await.unwrap();
        assert_eq!(transactions.next().await.unwrap(), 42);
        assert!(transactions.next().await.is_none());
        reply.await.unwrap();
    }

    #[tokio::test]
    async fn get_block_bytes_closed_channels() {
        let (storage, receiver) = api::<()>();
        drop(receiver);
        assert!(
            storage
                .get_block_bytes(&HeaderId::from([1; 32]))
                .await
                .is_err()
        );

        let (api, mut receiver) = api::<()>();
        let reply = tokio::spawn(async move {
            drop(receiver.recv().await.unwrap());
        });
        assert!(api.get_block_bytes(&HeaderId::from([1; 32])).await.is_err());
        reply.await.unwrap();
    }
}
