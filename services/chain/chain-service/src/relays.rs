use std::fmt::{Debug, Display};

use logos_blockchain_chain_broadcast_service::{BlockBroadcastMsg, BlockBroadcastService};
use bytes::Bytes;
use logos_blockchain_core::{
    block::Block,
    mantle::{AuthenticatedMantleTx, Transaction, TxHash},
};
use logos_blockchain_storage_service::{
    StorageMsg, StorageService, api::chain::StorageChainApi, backends::StorageBackend,
};
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{AsServiceId, relay::OutboundRelay},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    CryptarchiaConsensus,
    storage::{StorageAdapter as _, adapters::StorageAdapter},
};

pub type BroadcastRelay = OutboundRelay<BlockBroadcastMsg>;

pub type StorageRelay<Storage> = OutboundRelay<StorageMsg<Storage>>;

pub struct CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>
where
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
{
    broadcast_relay: BroadcastRelay,
    storage_adapter: StorageAdapter<Storage, Tx, RuntimeServiceId>,
}

impl<Tx, Storage, RuntimeServiceId> CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>
where
    Tx: Transaction<Hash = TxHash>
        + AuthenticatedMantleTx
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>>,
{
    pub async fn new(
        broadcast_relay: BroadcastRelay,
        storage_relay: StorageRelay<Storage>,
    ) -> Self {
        let storage_adapter =
            StorageAdapter::<Storage, Tx, RuntimeServiceId>::new(storage_relay).await;
        Self {
            broadcast_relay,
            storage_adapter,
        }
    }

    #[expect(clippy::allow_attributes_without_reason)]
    pub async fn from_service_resources_handle(
        service_resources_handle: &OpaqueServiceResourcesHandle<
            CryptarchiaConsensus<Tx, Storage, RuntimeServiceId>,
            RuntimeServiceId,
        >,
    ) -> Self
    where
        RuntimeServiceId: Debug
            + Sync
            + Send
            + Display
            + 'static
            + AsServiceId<BlockBroadcastService<RuntimeServiceId>>
            + AsServiceId<StorageService<Storage, RuntimeServiceId>>,
    {
        let broadcast_relay = service_resources_handle
            .overwatch_handle
            .relay::<BlockBroadcastService<_>>()
            .await
            .expect(
                "Relay connection with logos_blockchain_broadcast_service::BlockBroadcastService should
        succeed",
            );

        let storage_relay = service_resources_handle
            .overwatch_handle
            .relay::<StorageService<_, _>>()
            .await
            .expect("Relay connection with StorageService should succeed");

        Self::new(broadcast_relay, storage_relay).await
    }

    pub const fn broadcast_relay(&self) -> &BroadcastRelay {
        &self.broadcast_relay
    }

    pub const fn storage_adapter(&self) -> &StorageAdapter<Storage, Tx, RuntimeServiceId> {
        &self.storage_adapter
    }
}
