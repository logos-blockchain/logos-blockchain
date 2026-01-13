use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    marker::PhantomData,
    pin::Pin,
};

use bytes::Bytes;
use cryptarchia_engine::Slot;
use futures::{Stream, StreamExt as _};
use nomos_core::{
    block::Block,
    codec::{DeserializeOp as _, SerializeOp as _},
    header::HeaderId,
    mantle::{Transaction, TxHash},
};
use nomos_storage::{
    StorageMsg, StorageService, api::chain::StorageChainApi, backends::StorageBackend,
};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;

use crate::storage::StorageAdapter as StorageAdapterTrait;

pub struct StorageAdapter<Storage, Tx, RuntimeServiceId>
where
    Storage: StorageBackend + Send + Sync + 'static,
{
    pub storage_relay:
        OutboundRelay<<StorageService<Storage, RuntimeServiceId> as ServiceData>::Message>,
    _tx: PhantomData<Tx>,
}

impl<Storage, Tx, RuntimeServiceId> Clone for StorageAdapter<Storage, Tx, RuntimeServiceId>
where
    Storage: StorageBackend + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            storage_relay: self.storage_relay.clone(),
            _tx: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<Storage, Tx, RuntimeServiceId> StorageAdapterTrait<RuntimeServiceId>
    for StorageAdapter<Storage, Tx, RuntimeServiceId>
where
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>>,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    Tx: Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static
        + Transaction<Hash = TxHash>,
{
    type Backend = Storage;
    type Block = Block<Tx>;
    type Tx = Tx;

    async fn new(
        storage_relay: OutboundRelay<
            <StorageService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        Self {
            storage_relay,
            _tx: PhantomData,
        }
    }

    async fn get_block(&self, header_id: &HeaderId) -> Option<Self::Block> {
        let (sender, receiver) = oneshot::channel();

        self.storage_relay
            .send(StorageMsg::get_block_request(*header_id, sender))
            .await
            .unwrap();

        if let Ok(maybe_block) = receiver.await {
            let block = maybe_block?;
            block.try_into().ok()
        } else {
            tracing::error!("Failed to receive block from storage relay");
            None
        }
    }

    async fn store_block(
        &self,
        header_id: HeaderId,
        block: Self::Block,
    ) -> Result<(), overwatch::DynError> {
        let block = block
            .try_into()
            .map_err(|_| "Failed to convert block to storage format")?;

        self.storage_relay
            .send(StorageMsg::store_block_request(header_id, block))
            .await
            .map_err(|_| "Failed to send store block request to storage relay")?;

        Ok(())
    }

    async fn remove_block(
        &self,
        header_id: HeaderId,
    ) -> Result<Option<Self::Block>, overwatch::DynError> {
        let (sender, receiver) = oneshot::channel();

        self.storage_relay
            .send(StorageMsg::remove_block_request(header_id, sender))
            .await
            .map_err(|_| "Failed to send remove block request to storage relay.")?;

        let Some(removed_block) = receiver
            .await
            .map_err(|_| "No block was deleted from the storage.")?
        else {
            return Ok(None);
        };

        let deserialized_block = removed_block
            .try_into()
            .map_err(|_| "Failed to convert block to storage format.")?;

        Ok(Some(deserialized_block))
    }

    async fn store_immutable_block_ids(
        &self,
        blocks: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), overwatch::DynError> {
        self.storage_relay
            .send(StorageMsg::store_immutable_block_ids_request(blocks))
            .await
            .map_err(|_| "Failed to send store_immutable_block_id request to storage relay")?;
        Ok(())
    }

    async fn store_transactions(
        &self,
        transactions: Vec<Self::Tx>,
    ) -> Result<(), overwatch::DynError> {
        let storage_transactions: HashMap<TxHash, <Storage as StorageChainApi>::Tx> = transactions
            .into_iter()
            .map(|tx| {
                let hash = tx.hash();
                Tx::to_bytes(&tx)
                    .map(|bytes| (hash, bytes.into()))
                    .map_err(|_| "Failed to convert transaction to storage format".into())
            })
            .collect::<Result<HashMap<_, _>, overwatch::DynError>>()?;

        self.storage_relay
            .send(StorageMsg::store_transactions_request(storage_transactions))
            .await
            .map_err(|_| "Failed to send store transactions batch request")?;

        Ok(())
    }

    async fn get_transactions(
        &self,
        tx_hashes: BTreeSet<TxHash>,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Tx> + Send>>, overwatch::DynError> {
        let (sender, receiver) = oneshot::channel();

        self.storage_relay
            .send(StorageMsg::get_transactions_request(tx_hashes, sender))
            .await
            .map_err(|_| "Failed to send get transactions request")?;

        let storage_stream = receiver
            .await
            .map_err(|_| "Failed to receive transactions stream from storage")?;

        let mapped_stream =
            storage_stream.filter_map(async |storage_tx| Tx::from_bytes(storage_tx.as_ref()).ok());

        Ok(Box::pin(mapped_stream))
    }

    async fn remove_transactions(&self, tx_hashes: &[TxHash]) -> Result<(), overwatch::DynError> {
        self.storage_relay
            .send(StorageMsg::remove_transactions_request(tx_hashes.to_vec()))
            .await
            .map_err(|_| "Failed to send remove transactions batch request")?;

        Ok(())
    }
}
