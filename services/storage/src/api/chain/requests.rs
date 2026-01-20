use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Display,
    num::NonZeroUsize,
    ops::RangeInclusive,
    pin::Pin,
};

use futures::Stream;
use lb_core::{header::HeaderId, mantle::TxHash};
use lb_cryptarchia_engine::Slot;
use tokio::sync::oneshot::Sender;

use crate::{
    StorageMsg, StorageServiceError,
    api::{StorageApiRequest, StorageBackendApi, StorageOperation, chain::StorageChainApi},
    backends::StorageBackend,
};

pub enum ChainApiRequest<Backend: StorageBackend> {
    GetBlock {
        header_id: HeaderId,
        response_tx: Sender<Option<<Backend as StorageChainApi>::Block>>,
    },
    StoreBlock {
        header_id: HeaderId,
        block: <Backend as StorageChainApi>::Block,
    },
    RemoveBlock {
        header_id: HeaderId,
        response_tx: Sender<Option<Backend::Block>>,
    },
    StoreImmutableBlockIds {
        ids: BTreeMap<Slot, HeaderId>,
    },
    GetImmutableBlockId {
        slot: Slot,
        response_tx: Sender<Option<HeaderId>>,
    },
    ScanImmutableBlockIds {
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
        response_tx: Sender<Vec<HeaderId>>,
    },
    StoreTransactions {
        transactions: HashMap<TxHash, <Backend as StorageChainApi>::Tx>,
    },
    GetTransactions {
        tx_hashes: BTreeSet<TxHash>,
        response_tx: Sender<Pin<Box<dyn Stream<Item = <Backend as StorageChainApi>::Tx> + Send>>>,
    },
    RemoveTransactions {
        tx_hashes: Vec<TxHash>,
    },
}

impl<Backend> StorageOperation<Backend> for ChainApiRequest<Backend>
where
    Backend: StorageBackend + StorageBackendApi,
{
    async fn execute(self, backend: &mut Backend) -> Result<(), StorageServiceError> {
        match self {
            Self::GetBlock {
                header_id,
                response_tx,
            } => handle_get_block(backend, header_id, response_tx).await,
            Self::StoreBlock { header_id, block } => {
                handle_store_block(backend, header_id, block).await
            }
            Self::RemoveBlock {
                header_id,
                response_tx,
            } => handle_remove_block(backend, header_id, response_tx).await,
            Self::StoreImmutableBlockIds { ids: block_ids } => {
                handle_store_immutable_block_ids(backend, block_ids).await
            }
            Self::GetImmutableBlockId { slot, response_tx } => {
                handle_get_immutable_block_id(backend, slot, response_tx).await
            }
            Self::ScanImmutableBlockIds {
                slot_range,
                limit,
                response_tx,
            } => handle_scan_immutable_block_ids(backend, slot_range, limit, response_tx).await,
            Self::StoreTransactions { transactions } => {
                handle_store_transactions(backend, transactions).await
            }
            Self::GetTransactions {
                tx_hashes,
                response_tx,
            } => handle_get_transactions(backend, tx_hashes, response_tx).await,
            Self::RemoveTransactions { tx_hashes } => {
                handle_remove_transactions(backend, tx_hashes).await
            }
        }
    }
}

async fn handle_get_block<Backend: StorageBackend>(
    backend: &mut Backend,
    header_id: HeaderId,
    response_tx: Sender<Option<Backend::Block>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .get_block(header_id)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: format!(
                "Failed to send reply for get block request by header_id: {header_id}"
            ),
        });
    }

    Ok(())
}

async fn handle_store_block<Backend: StorageBackend>(
    backend: &mut Backend,
    header_id: HeaderId,
    block: Backend::Block,
) -> Result<(), StorageServiceError> {
    backend
        .store_block(header_id, block)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))
}

async fn handle_remove_block<B>(
    backend: &mut B,
    header_id: HeaderId,
    response_tx: Sender<Option<B::Block>>,
) -> Result<(), StorageServiceError>
where
    B: StorageBackend<Error: Display>,
{
    let result = backend
        .remove_block(header_id)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    response_tx
        .send(result)
        .map_err(|_| StorageServiceError::ReplyError {
            message: format!(
                "Failed to send reply for remove block request by header_id: {header_id}"
            ),
        })
}

async fn handle_store_immutable_block_ids<Backend: StorageBackend>(
    backend: &mut Backend,
    ids: BTreeMap<Slot, HeaderId>,
) -> Result<(), StorageServiceError> {
    backend
        .store_immutable_block_ids(ids)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))
}

async fn handle_get_immutable_block_id<Backend: StorageBackend>(
    backend: &mut Backend,
    slot: Slot,
    response_tx: Sender<Option<HeaderId>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .get_immutable_block_id(slot)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: format!(
                "Failed to send reply for get_immutable_block_id request for slot:{slot:?}"
            ),
        });
    }

    Ok(())
}

async fn handle_scan_immutable_block_ids<Backend: StorageBackend>(
    backend: &mut Backend,
    slot_range: RangeInclusive<Slot>,
    limit: NonZeroUsize,
    response_tx: Sender<Vec<HeaderId>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .scan_immutable_block_ids(slot_range, limit)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for scan_immutable_block_ids request".into(),
        });
    }

    Ok(())
}

impl<Api: StorageBackend> StorageMsg<Api> {
    #[must_use]
    pub const fn get_block_request(
        header_id: HeaderId,
        response_tx: Sender<Option<<Api as StorageChainApi>::Block>>,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::GetBlock {
                header_id,
                response_tx,
            }),
        }
    }

    pub const fn store_block_request(
        header_id: HeaderId,
        block: <Api as StorageChainApi>::Block,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::StoreBlock { header_id, block }),
        }
    }

    #[must_use]
    pub const fn remove_block_request(
        header_id: HeaderId,
        response_tx: Sender<Option<Api::Block>>,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::RemoveBlock {
                header_id,
                response_tx,
            }),
        }
    }

    #[must_use]
    pub const fn store_immutable_block_ids_request(ids: BTreeMap<Slot, HeaderId>) -> Self {
        Self::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::StoreImmutableBlockIds { ids }),
        }
    }

    #[must_use]
    pub const fn get_immutable_block_id_request(
        slot: Slot,
        response_tx: Sender<Option<HeaderId>>,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::GetImmutableBlockId {
                slot,
                response_tx,
            }),
        }
    }

    #[must_use]
    pub const fn scan_immutable_block_ids_request(
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
        response_tx: Sender<Vec<HeaderId>>,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::ScanImmutableBlockIds {
                slot_range,
                limit,
                response_tx,
            }),
        }
    }

    #[must_use]
    pub const fn store_transactions_request(
        transactions: HashMap<TxHash, <Api as StorageChainApi>::Tx>,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::StoreTransactions { transactions }),
        }
    }

    #[must_use]
    pub const fn get_transactions_request(
        tx_hashes: BTreeSet<TxHash>,
        response_tx: Sender<Pin<Box<dyn Stream<Item = <Api as StorageChainApi>::Tx> + Send>>>,
    ) -> Self {
        Self::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::GetTransactions {
                tx_hashes,
                response_tx,
            }),
        }
    }

    #[must_use]
    pub const fn remove_transactions_request(tx_hashes: Vec<TxHash>) -> Self {
        Self::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::RemoveTransactions { tx_hashes }),
        }
    }
}

async fn handle_store_transactions<Backend: StorageBackend>(
    backend: &mut Backend,
    transactions: HashMap<TxHash, <Backend as StorageChainApi>::Tx>,
) -> Result<(), StorageServiceError> {
    backend
        .store_transactions(transactions)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    Ok(())
}

async fn handle_get_transactions<Backend: StorageBackend>(
    backend: &mut Backend,
    tx_hashes: BTreeSet<TxHash>,
    response_tx: Sender<Pin<Box<dyn Stream<Item = <Backend as StorageChainApi>::Tx> + Send>>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .get_transactions(tx_hashes)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for get transactions batch request".to_owned(),
        });
    }

    Ok(())
}

async fn handle_remove_transactions<Backend: StorageBackend>(
    backend: &mut Backend,
    tx_hashes: Vec<TxHash>,
) -> Result<(), StorageServiceError> {
    backend
        .remove_transactions(&tx_hashes)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    Ok(())
}
