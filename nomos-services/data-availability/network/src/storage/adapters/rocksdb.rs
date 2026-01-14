use std::collections::HashMap;

use libp2p::{Multiaddr, PeerId};
use nomos_core::sdp::{ProviderId, SessionNumber};
use nomos_da_network_core::SubnetworkId;
use nomos_storage::{StorageMsg, StorageService, backends::StorageBackend};
use overwatch::{DynError, services::relay::OutboundRelay};

use crate::{membership::Assignations, storage::MembershipStorageAdapter};

pub struct RocksAdapter<Backend, RuntimeServiceId>
where
    Backend: StorageBackend + Send + Sync + 'static,
{
    storage_relay: OutboundRelay<
        <StorageService<Backend, RuntimeServiceId> as overwatch::services::ServiceData>::Message,
    >,
}

#[async_trait::async_trait]
impl<Backend, RuntimeServiceId> MembershipStorageAdapter<PeerId, SubnetworkId>
    for RocksAdapter<Backend, RuntimeServiceId>
where
    Backend: StorageBackend<Id = PeerId, NetworkId = SubnetworkId> + Send + Sync + 'static,
{
    type StorageService = StorageService<Backend, RuntimeServiceId>;

    fn new(
        relay: OutboundRelay<<Self::StorageService as overwatch::services::ServiceData>::Message>,
    ) -> Self {
        Self {
            storage_relay: relay,
        }
    }

    async fn store(
        &self,
        session_id: SessionNumber,
        assignations: Assignations<PeerId, SubnetworkId>,
        mappings: HashMap<PeerId, ProviderId>,
    ) -> Result<(), DynError> {
        let store_assignations_msg =
            StorageMsg::store_assignations_request(session_id, assignations);
        self.storage_relay
            .send(store_assignations_msg)
            .await
            .map_err(|(e, _)| DynError::from(e))?;

        // Store provider mappings
        let store_mappings_msg = StorageMsg::store_provider_mappings_request(mappings);
        self.storage_relay
            .send(store_mappings_msg)
            .await
            .map_err(|(e, _)| DynError::from(e))?;

        Ok(())
    }

    async fn get(
        &self,
        session_id: SessionNumber,
    ) -> Result<Option<Assignations<PeerId, SubnetworkId>>, DynError> {
        let (reply_channel, reply_rx) = tokio::sync::oneshot::channel();
        let get_assignations_msg = StorageMsg::get_assignations_request(session_id, reply_channel);
        self.storage_relay
            .send(get_assignations_msg)
            .await
            .map_err(|(e, _)| DynError::from(e))?;
        reply_rx.await.map_err(DynError::from)
    }

    async fn store_addresses(&self, ids: HashMap<PeerId, Multiaddr>) -> Result<(), DynError> {
        let store_ids_msg = StorageMsg::store_addresses_request(ids);
        self.storage_relay
            .send(store_ids_msg)
            .await
            .map_err(|(e, _)| DynError::from(e))?;
        Ok(())
    }

    async fn get_address(&self, id: PeerId) -> Result<Option<Multiaddr>, DynError> {
        let (reply_channel, reply_rx) = tokio::sync::oneshot::channel();
        let get_address_msg = StorageMsg::get_address_request(id, reply_channel);
        self.storage_relay
            .send(get_address_msg)
            .await
            .map_err(|(e, _)| DynError::from(e))?;
        reply_rx.await.map_err(DynError::from)
    }

    async fn get_provider_id(&self, id: PeerId) -> Result<Option<ProviderId>, DynError> {
        let (reply_channel, reply_rx) = tokio::sync::oneshot::channel();
        let get_provider_id_msg = StorageMsg::get_provider_id_request(id, reply_channel);
        self.storage_relay
            .send(get_provider_id_msg)
            .await
            .map_err(|(e, _)| DynError::from(e))?;
        reply_rx.await.map_err(DynError::from)
    }

    async fn prune(&self, cutoff_session: SessionNumber) -> Result<(), DynError> {
        let prune_msg = StorageMsg::prune_assignations_request(cutoff_session);
        self.storage_relay
            .send(prune_msg)
            .await
            .map_err(|(e, _)| DynError::from(e))?;
        Ok(())
    }
}
