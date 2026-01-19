use std::collections::{BTreeSet, HashMap};

use async_trait::async_trait;
use lb_core::{
    block::BlockNumber,
    codec::{DeserializeOp as _, SerializeOp as _},
    sdp::{Locator, ProviderId, ServiceType, SessionNumber},
};
use overwatch::DynError;
use tracing::{debug, error};

use crate::{
    api::{backend::rocksdb::utils::key_bytes, membership::StorageMembershipApi},
    backends::{StorageBackend as _, rocksdb::RocksBackend},
};

pub const MEMBERSHIP_ACTIVE_SESSION_PREFIX: &str = "membership/active/";
pub const MEMBERSHIP_FORMING_SESSION_PREFIX: &str = "membership/forming/";
pub const MEMBERSHIP_LATEST_BLOCK_KEY: &str = "membership/latest_block";

type MembershipProviders = (SessionNumber, HashMap<ProviderId, BTreeSet<Locator>>);

#[async_trait]
impl StorageMembershipApi for RocksBackend {
    async fn save_active_session(
        &mut self,
        service_type: ServiceType,
        session_id: SessionNumber,
        providers: &HashMap<ProviderId, BTreeSet<Locator>>,
    ) -> Result<(), DynError> {
        let service_bytes = service_type
            .to_bytes()
            .expect("Serialization of ServiceType should not fail");
        let key = key_bytes(MEMBERSHIP_ACTIVE_SESSION_PREFIX, service_bytes);

        let session_data = (session_id, providers);
        let serialized_data = session_data
            .to_bytes()
            .expect("Serialization of session data should not fail");

        match self.store(key, serialized_data).await {
            Ok(()) => {
                debug!(
                    "Successfully stored active session {} for service {:?}",
                    session_id, service_type
                );
                Ok(())
            }
            Err(e) => {
                error!("Failed to store active session: {:?}", e);
                Err(e.into())
            }
        }
    }

    async fn load_active_session(
        &mut self,
        service_type: ServiceType,
    ) -> Result<Option<MembershipProviders>, DynError> {
        let service_bytes = service_type
            .to_bytes()
            .expect("Serialization of ServiceType should not fail");
        let key = key_bytes(MEMBERSHIP_ACTIVE_SESSION_PREFIX, service_bytes);

        let data = self.load(&key).await?;

        data.map_or_else(
            || {
                debug!("No active session found for service {:?}", service_type);
                Ok(None)
            },
            |bytes| match MembershipProviders::from_bytes(&bytes) {
                Ok(session_data) => {
                    debug!(
                        "Successfully loaded active session for service {:?}",
                        service_type
                    );
                    Ok(Some(session_data))
                }
                Err(e) => {
                    error!("Failed to deserialize active session: {:?}", e);
                    Ok(None)
                }
            },
        )
    }

    async fn save_latest_block(&mut self, block_number: BlockNumber) -> Result<(), DynError> {
        let block_bytes = block_number.to_le_bytes();

        match self
            .store(
                MEMBERSHIP_LATEST_BLOCK_KEY.into(),
                block_bytes.to_vec().into(),
            )
            .await
        {
            Ok(()) => {
                debug!("Successfully stored latest block {}", block_number);
                Ok(())
            }
            Err(e) => {
                error!("Failed to store latest block: {:?}", e);
                Err(e.into())
            }
        }
    }

    async fn load_latest_block(&mut self) -> Result<Option<BlockNumber>, DynError> {
        let data = self.load(MEMBERSHIP_LATEST_BLOCK_KEY.as_bytes()).await?;

        match data {
            None => {
                debug!("No latest block found");
                Ok(None)
            }
            Some(bytes) => {
                if bytes.len() != 8 {
                    error!("Invalid block number bytes length: {}", bytes.len());
                    return Ok(None);
                }

                let block_bytes: [u8; 8] = bytes[..8].try_into().unwrap();
                let block_number = BlockNumber::from_le_bytes(block_bytes);
                debug!("Successfully loaded latest block {}", block_number);
                Ok(Some(block_number))
            }
        }
    }

    async fn save_forming_session(
        &mut self,
        service_type: ServiceType,
        session_id: SessionNumber,
        providers: &HashMap<ProviderId, BTreeSet<Locator>>,
    ) -> Result<(), DynError> {
        let service_bytes = service_type
            .to_bytes()
            .expect("Serialization of ServiceType should not fail");
        let key = key_bytes(MEMBERSHIP_FORMING_SESSION_PREFIX, service_bytes);

        let session_data = (session_id, providers);
        let serialized_data = session_data
            .to_bytes()
            .expect("Serialization of session data should not fail");

        match self.store(key, serialized_data).await {
            Ok(()) => {
                debug!(
                    "Successfully stored forming session {} for service {:?}",
                    session_id, service_type
                );
                Ok(())
            }
            Err(e) => {
                error!("Failed to store forming session: {:?}", e);
                Err(e.into())
            }
        }
    }

    async fn load_forming_session(
        &mut self,
        service_type: ServiceType,
    ) -> Result<Option<MembershipProviders>, DynError> {
        let service_bytes = service_type
            .to_bytes()
            .expect("Serialization of ServiceType should not fail");
        let key = key_bytes(MEMBERSHIP_FORMING_SESSION_PREFIX, service_bytes);

        let data = self.load(&key).await?;

        data.map_or_else(
            || {
                debug!("No forming session found for service {:?}", service_type);
                Ok(None)
            },
            |bytes| match <MembershipProviders>::from_bytes(&bytes) {
                Ok(session_data) => {
                    debug!(
                        "Successfully loaded forming session for service {:?}",
                        service_type
                    );
                    Ok(Some(session_data))
                }
                Err(e) => {
                    error!("Failed to deserialize forming session: {:?}", e);
                    Ok(None)
                }
            },
        )
    }
}
