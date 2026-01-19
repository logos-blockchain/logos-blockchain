use std::collections::{BTreeSet, HashMap};

use lb_core::{
    block::BlockNumber,
    sdp::{Locator, ProviderId, ServiceType, SessionNumber},
};
use tokio::sync::oneshot::Sender;

use crate::{
    StorageMsg, StorageServiceError,
    api::{
        StorageApiRequest, StorageBackendApi, StorageOperation, membership::StorageMembershipApi,
    },
    backends::StorageBackend,
};

pub type SessionSender = Sender<Option<(SessionNumber, HashMap<ProviderId, BTreeSet<Locator>>)>>;

pub enum MembershipApiRequest {
    SaveActiveSession {
        service_type: ServiceType,
        session_id: SessionNumber,
        providers: HashMap<ProviderId, BTreeSet<Locator>>,
    },
    LoadActiveSession {
        service_type: ServiceType,
        response_tx: SessionSender,
    },
    SaveLatestBlock {
        block_number: BlockNumber,
    },
    LoadLatestBlock {
        response_tx: Sender<Option<BlockNumber>>,
    },
    SaveFormingSession {
        service_type: ServiceType,
        session_id: SessionNumber,
        providers: HashMap<ProviderId, BTreeSet<Locator>>,
    },
    LoadFormingSession {
        service_type: ServiceType,
        response_tx: SessionSender,
    },
}

impl<Backend> StorageOperation<Backend> for MembershipApiRequest
where
    Backend: StorageBackend + StorageBackendApi + StorageMembershipApi,
{
    async fn execute(self, backend: &mut Backend) -> Result<(), StorageServiceError> {
        match self {
            Self::SaveActiveSession {
                service_type,
                session_id,
                providers,
            } => handle_save_active_session(backend, service_type, session_id, providers).await,
            Self::LoadActiveSession {
                service_type,
                response_tx,
            } => handle_load_active_session(backend, service_type, response_tx).await,
            Self::SaveLatestBlock { block_number } => {
                handle_save_latest_block(backend, block_number).await
            }
            Self::LoadLatestBlock { response_tx } => {
                handle_load_latest_block(backend, response_tx).await
            }
            Self::SaveFormingSession {
                service_type,
                session_id,
                providers,
            } => handle_save_forming_session(backend, service_type, session_id, providers).await,
            Self::LoadFormingSession {
                service_type,
                response_tx,
            } => handle_load_forming_session(backend, service_type, response_tx).await,
        }
    }
}

async fn handle_save_active_session<Backend: StorageBackend + StorageMembershipApi>(
    backend: &mut Backend,
    service_type: ServiceType,
    session_id: SessionNumber,
    providers: HashMap<ProviderId, BTreeSet<Locator>>,
) -> Result<(), StorageServiceError> {
    backend
        .save_active_session(service_type, session_id, &providers)
        .await
        .map_err(StorageServiceError::BackendError)
}

async fn handle_load_active_session<Backend: StorageBackend + StorageMembershipApi>(
    backend: &mut Backend,
    service_type: ServiceType,
    response_tx: SessionSender,
) -> Result<(), StorageServiceError> {
    let result = backend
        .load_active_session(service_type)
        .await
        .map_err(StorageServiceError::BackendError)?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for load active session request".to_owned(),
        });
    }
    Ok(())
}

async fn handle_save_latest_block<Backend: StorageBackend + StorageMembershipApi>(
    backend: &mut Backend,
    block_number: BlockNumber,
) -> Result<(), StorageServiceError> {
    backend
        .save_latest_block(block_number)
        .await
        .map_err(StorageServiceError::BackendError)
}

async fn handle_load_latest_block<Backend: StorageBackend + StorageMembershipApi>(
    backend: &mut Backend,
    response_tx: Sender<Option<BlockNumber>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .load_latest_block()
        .await
        .map_err(StorageServiceError::BackendError)?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for load latest block request".to_owned(),
        });
    }
    Ok(())
}

async fn handle_save_forming_session<Backend: StorageBackend + StorageMembershipApi>(
    backend: &mut Backend,
    service_type: ServiceType,
    session_id: SessionNumber,
    providers: HashMap<ProviderId, BTreeSet<Locator>>,
) -> Result<(), StorageServiceError> {
    backend
        .save_forming_session(service_type, session_id, &providers)
        .await
        .map_err(StorageServiceError::BackendError)
}

async fn handle_load_forming_session<Backend: StorageBackend + StorageMembershipApi>(
    backend: &mut Backend,
    service_type: ServiceType,
    response_tx: SessionSender,
) -> Result<(), StorageServiceError> {
    let result = backend
        .load_forming_session(service_type)
        .await
        .map_err(StorageServiceError::BackendError)?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for load forming session request".to_owned(),
        });
    }
    Ok(())
}

impl<Backend: StorageBackend> StorageMsg<Backend> {
    #[must_use]
    pub const fn save_active_session_request(
        service_type: ServiceType,
        session_id: SessionNumber,
        providers: HashMap<ProviderId, BTreeSet<Locator>>,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Membership(MembershipApiRequest::SaveActiveSession {
                service_type,
                session_id,
                providers,
            }),
        }
    }

    #[must_use]
    pub const fn load_active_session_request(
        service_type: ServiceType,
        response_tx: SessionSender,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Membership(MembershipApiRequest::LoadActiveSession {
                service_type,
                response_tx,
            }),
        }
    }

    #[must_use]
    pub const fn save_latest_block_request(block_number: BlockNumber) -> Self {
        Self::Api {
            request: StorageApiRequest::Membership(MembershipApiRequest::SaveLatestBlock {
                block_number,
            }),
        }
    }

    #[must_use]
    pub const fn load_latest_block_request(response_tx: Sender<Option<BlockNumber>>) -> Self {
        Self::Api {
            request: StorageApiRequest::Membership(MembershipApiRequest::LoadLatestBlock {
                response_tx,
            }),
        }
    }

    #[must_use]
    pub const fn save_forming_session_request(
        service_type: ServiceType,
        session_id: SessionNumber,
        providers: HashMap<ProviderId, BTreeSet<Locator>>,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Membership(MembershipApiRequest::SaveFormingSession {
                service_type,
                session_id,
                providers,
            }),
        }
    }

    #[must_use]
    pub const fn load_forming_session_request(
        service_type: ServiceType,
        response_tx: SessionSender,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Membership(MembershipApiRequest::LoadFormingSession {
                service_type,
                response_tx,
            }),
        }
    }
}
