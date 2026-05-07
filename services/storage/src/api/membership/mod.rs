use std::collections::{BTreeSet, HashMap};

use async_trait::async_trait;
use lb_core::{
    block::BlockNumber,
    sdp::{Locator, ProviderId, ServiceType, SessionNumber},
};
use overwatch::DynError;

pub mod requests;

#[async_trait]
pub trait StorageMembershipApi {
    async fn save_active_session(
        &mut self,
        service_type: ServiceType,
        session_id: SessionNumber,
        providers: &HashMap<ProviderId, BTreeSet<Locator>>,
    ) -> Result<(), DynError>;

    async fn load_active_session(
        &mut self,
        service_type: ServiceType,
    ) -> Result<Option<(SessionNumber, HashMap<ProviderId, BTreeSet<Locator>>)>, DynError>;

    async fn save_latest_block(&mut self, block_number: BlockNumber) -> Result<(), DynError>;

    async fn load_latest_block(&mut self) -> Result<Option<BlockNumber>, DynError>;

    async fn save_next_session(
        &mut self,
        service_type: ServiceType,
        session_id: SessionNumber,
        providers: &HashMap<ProviderId, BTreeSet<Locator>>,
    ) -> Result<(), DynError>;

    async fn load_next_session(
        &mut self,
        service_type: ServiceType,
    ) -> Result<Option<(SessionNumber, HashMap<ProviderId, BTreeSet<Locator>>)>, DynError>;
}
