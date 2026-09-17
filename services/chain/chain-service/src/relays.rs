use std::fmt::{Debug, Display};

use lb_chain_broadcast_service::{BlockBroadcastMsg, BlockBroadcastService};
use lb_core::mantle::traits::{PreverifiedMantleTransaction, StorageSize};
use lb_storage_service::{StorageService, api::StorageApi};
use lb_time_service::{TimeService, TimeServiceMessage};
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{AsServiceId, relay::OutboundRelay},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::CryptarchiaConsensus;

pub type BroadcastRelay = OutboundRelay<BlockBroadcastMsg>;

pub type TimeRelay = OutboundRelay<TimeServiceMessage>;

pub struct CryptarchiaConsensusRelays<Tx> {
    broadcast_relay: BroadcastRelay,
    storage: StorageApi<Tx>,
    time_relay: TimeRelay,
}

impl<Tx> CryptarchiaConsensusRelays<Tx> {
    pub const fn new(
        broadcast_relay: BroadcastRelay,
        storage: StorageApi<Tx>,
        time_relay: TimeRelay,
    ) -> Self {
        Self {
            broadcast_relay,
            storage,
            time_relay,
        }
    }

    #[expect(clippy::allow_attributes_without_reason)]
    pub async fn from_service_resources_handle<TimeBackend, RuntimeServiceId>(
        service_resources_handle: &OpaqueServiceResourcesHandle<
            CryptarchiaConsensus<Tx, TimeBackend, RuntimeServiceId>,
            RuntimeServiceId,
        >,
    ) -> Self
    where
        Tx: PreverifiedMantleTransaction
            + Debug
            + Clone
            + Eq
            + Serialize
            + DeserializeOwned
            + Send
            + Sync
            + Unpin
            + 'static
            + StorageSize,
        TimeBackend: lb_time_service::backends::TimeBackend,
        TimeBackend::Settings: Clone + Send + Sync + 'static,
        RuntimeServiceId: Debug
            + Sync
            + Send
            + Display
            + 'static
            + AsServiceId<BlockBroadcastService<RuntimeServiceId>>
            + AsServiceId<StorageService<RuntimeServiceId>>
            + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>,
    {
        let broadcast_relay = service_resources_handle
            .overwatch_handle
            .relay::<BlockBroadcastService<_>>()
            .await
            .expect(
                "Relay connection with lb_chain_broadcast_service::BlockBroadcastService should
        succeed",
            );

        let storage = StorageApi::from_overwatch_handle(&service_resources_handle.overwatch_handle)
            .await
            .expect("Relay connection with StorageService should succeed");

        let time_relay = service_resources_handle
            .overwatch_handle
            .relay::<TimeService<_, _>>()
            .await
            .expect("Relay connection with TimeService should succeed");

        Self::new(broadcast_relay, storage, time_relay)
    }

    pub const fn broadcast_relay(&self) -> &BroadcastRelay {
        &self.broadcast_relay
    }

    pub const fn storage(&self) -> &StorageApi<Tx> {
        &self.storage
    }

    pub const fn time_relay(&self) -> &TimeRelay {
        &self.time_relay
    }
}
