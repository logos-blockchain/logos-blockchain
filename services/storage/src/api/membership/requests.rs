use lb_core::block::BlockNumber;
use tokio::sync::oneshot::Sender;

use crate::{
    StorageServiceError,
    api::{StorageBackendApi, StorageOperation, membership::StorageMembershipApi},
    backends::StorageBackend,
};

pub enum MembershipApiRequest {
    SaveLatestBlock {
        block_number: BlockNumber,
    },
    LoadLatestBlock {
        response_tx: Sender<Option<BlockNumber>>,
    },
}

impl<Backend> StorageOperation<Backend> for MembershipApiRequest
where
    Backend: StorageBackend + StorageBackendApi + StorageMembershipApi,
{
    async fn execute(self, backend: &mut Backend) -> Result<(), StorageServiceError> {
        match self {
            Self::SaveLatestBlock { block_number } => {
                handle_save_latest_block(backend, block_number).await
            }
            Self::LoadLatestBlock { response_tx } => {
                handle_load_latest_block(backend, response_tx).await
            }
        }
    }
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
