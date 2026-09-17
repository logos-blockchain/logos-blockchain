use std::{
    collections::{BTreeMap, HashMap},
    num::NonZeroUsize,
    ops::RangeInclusive,
    pin::Pin,
};

use bytes::Bytes;
use futures::Stream;
use lb_core::{header::HeaderId, mantle::TxHash};
use lb_cryptarchia_engine::Slot;
use tokio::sync::oneshot::Sender;

use crate::{StorageMsg, StorageReplyReceiver};
#[cfg(feature = "rocksdb-backend")]
use crate::{
    StorageServiceError,
    api::backend::{streamed_immutable_block_ids_reverse_vec, streamed_immutable_block_ids_vec},
    backends::rocksdb::RocksBackend,
};

pub enum ChainApiRequest {
    GetBlock {
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    },
    StoreBlockData {
        header_id: HeaderId,
        parent_id: HeaderId,
        block: Bytes,
        events: Bytes,
        immutable_ids: BTreeMap<Slot, HeaderId>,
        response_tx: Sender<Result<(), String>>,
    },
    RemoveBlock {
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    },
    GetBlockParent {
        header_id: HeaderId,
        response_tx: Sender<Option<HeaderId>>,
    },
    GetBlockEvents {
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    },
    StoreImmutableBlockIds {
        ids: BTreeMap<Slot, HeaderId>,
        response_tx: Sender<Result<(), String>>,
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
    ScanImmutableBlockIdsReverse {
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
        response_tx: Sender<Vec<HeaderId>>,
    },
    StoreTransactions {
        transactions: HashMap<TxHash, Bytes>,
    },
    GetTransactions {
        tx_hashes: Vec<TxHash>,
        response_tx: Sender<Pin<Box<dyn Stream<Item = Bytes> + Send>>>,
    },
    RemoveTransactions {
        tx_hashes: Vec<TxHash>,
    },
}

#[cfg(feature = "rocksdb-backend")]
impl ChainApiRequest {
    pub(crate) async fn execute(
        self,
        backend: &mut RocksBackend,
    ) -> Result<(), StorageServiceError> {
        match self {
            Self::GetBlock {
                header_id,
                response_tx,
            } => handle_get_block(backend, header_id, response_tx).await,
            Self::StoreBlockData {
                header_id,
                parent_id,
                block,
                events,
                immutable_ids,
                response_tx,
            } => {
                handle_store_block_data(
                    backend,
                    header_id,
                    parent_id,
                    block,
                    events,
                    immutable_ids,
                    response_tx,
                )
                .await
            }
            Self::RemoveBlock {
                header_id,
                response_tx,
            } => handle_remove_block(backend, header_id, response_tx).await,
            Self::GetBlockParent {
                header_id,
                response_tx,
            } => handle_get_block_parent(backend, header_id, response_tx).await,
            Self::GetBlockEvents {
                header_id,
                response_tx,
            } => handle_get_block_events(backend, header_id, response_tx).await,
            Self::StoreImmutableBlockIds {
                ids: block_ids,
                response_tx,
            } => handle_store_immutable_block_ids(backend, block_ids, response_tx).await,
            Self::GetImmutableBlockId { slot, response_tx } => {
                handle_get_immutable_block_id(backend, slot, response_tx).await
            }
            Self::ScanImmutableBlockIds {
                slot_range,
                limit,
                response_tx,
            } => handle_scan_immutable_block_ids(backend, slot_range, limit, response_tx).await,
            Self::ScanImmutableBlockIdsReverse {
                slot_range,
                limit,
                response_tx,
            } => {
                handle_scan_immutable_block_ids_reverse(backend, slot_range, limit, response_tx)
                    .await
            }
            Self::StoreTransactions { transactions } => {
                handle_store_transactions(backend, transactions).await
            }
            Self::GetTransactions {
                tx_hashes,
                response_tx,
            } => handle_get_transactions(backend, tx_hashes, response_tx),
            Self::RemoveTransactions { tx_hashes } => {
                handle_remove_transactions(backend, tx_hashes).await
            }
        }
    }
}

#[cfg(feature = "rocksdb-backend")]
async fn handle_get_block(
    backend: &mut RocksBackend,
    header_id: HeaderId,
    response_tx: Sender<Option<Bytes>>,
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

#[cfg(feature = "rocksdb-backend")]
async fn handle_store_block_data(
    backend: &mut RocksBackend,
    header_id: HeaderId,
    parent_id: HeaderId,
    block: Bytes,
    events: Bytes,
    immutable_ids: BTreeMap<Slot, HeaderId>,
    response_tx: Sender<Result<(), String>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .store_block_data(header_id, parent_id, block, events, immutable_ids)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()));

    let response = result.as_ref().map_err(ToString::to_string).copied();

    response_tx
        .send(response)
        .map_err(|_| StorageServiceError::ReplyError {
            message: format!(
                "Failed to send reply for store block data request by header_id: {header_id}"
            ),
        })?;

    result
}

#[cfg(feature = "rocksdb-backend")]
async fn handle_get_block_parent(
    backend: &mut RocksBackend,
    header_id: HeaderId,
    response_tx: Sender<Option<HeaderId>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .get_block_parent(header_id)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: format!(
                "Failed to send reply for get block parent request by header_id: {header_id}"
            ),
        });
    }

    Ok(())
}

#[cfg(feature = "rocksdb-backend")]
async fn handle_get_block_events(
    backend: &mut RocksBackend,
    header_id: HeaderId,
    response_tx: Sender<Option<Bytes>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .get_block_events(header_id)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: format!(
                "Failed to send reply for get block events request by header_id: {header_id}"
            ),
        });
    }

    Ok(())
}

#[cfg(feature = "rocksdb-backend")]
async fn handle_remove_block(
    backend: &mut RocksBackend,
    header_id: HeaderId,
    response_tx: Sender<Option<Bytes>>,
) -> Result<(), StorageServiceError> {
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

#[cfg(feature = "rocksdb-backend")]
async fn handle_store_immutable_block_ids(
    backend: &mut RocksBackend,
    ids: BTreeMap<Slot, HeaderId>,
    response_tx: Sender<Result<(), String>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .store_immutable_block_ids(ids)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()));

    let response = result.as_ref().map_err(ToString::to_string).copied();

    response_tx
        .send(response)
        .map_err(|_| StorageServiceError::ReplyError {
            message: "Failed to send reply for store immutable block ids request".to_owned(),
        })?;

    result
}

#[cfg(feature = "rocksdb-backend")]
async fn handle_get_immutable_block_id(
    backend: &mut RocksBackend,
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

#[cfg(feature = "rocksdb-backend")]
async fn handle_scan_immutable_block_ids(
    backend: &mut RocksBackend,
    slot_range: RangeInclusive<Slot>,
    limit: NonZeroUsize,
    response_tx: Sender<Vec<HeaderId>>,
) -> Result<(), StorageServiceError> {
    let result = streamed_immutable_block_ids_vec(backend, slot_range, limit).await?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for scan_immutable_block_ids request".into(),
        });
    }

    Ok(())
}
#[cfg(feature = "rocksdb-backend")]
async fn handle_scan_immutable_block_ids_reverse(
    backend: &mut RocksBackend,
    slot_range: RangeInclusive<Slot>,
    limit: NonZeroUsize,
    response_tx: Sender<Vec<HeaderId>>,
) -> Result<(), StorageServiceError> {
    let result = streamed_immutable_block_ids_reverse_vec(backend, slot_range, limit).await?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for scan_immutable_block_ids_reverse request".into(),
        });
    }

    Ok(())
}

impl StorageMsg {
    pub fn new_load_message(key: Bytes) -> (Self, StorageReplyReceiver<Option<Bytes>>) {
        let (reply_channel, receiver) = tokio::sync::oneshot::channel();
        (
            Self::Load { key, reply_channel },
            StorageReplyReceiver::new(receiver),
        )
    }

    #[must_use]
    pub const fn get_block_request(
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    ) -> Self {
        Self::Api {
            request: ChainApiRequest::GetBlock {
                header_id,
                response_tx,
            },
        }
    }

    pub const fn store_block_data_request(
        header_id: HeaderId,
        parent_id: HeaderId,
        block: Bytes,
        events: Bytes,
        immutable_ids: BTreeMap<Slot, HeaderId>,
        response_tx: Sender<Result<(), String>>,
    ) -> Self {
        Self::Api {
            request: ChainApiRequest::StoreBlockData {
                header_id,
                parent_id,
                block,
                events,
                immutable_ids,
                response_tx,
            },
        }
    }

    #[must_use]
    pub const fn get_block_parent_request(
        header_id: HeaderId,
        response_tx: Sender<Option<HeaderId>>,
    ) -> Self {
        Self::Api {
            request: ChainApiRequest::GetBlockParent {
                header_id,
                response_tx,
            },
        }
    }

    #[must_use]
    pub const fn get_block_events_request(
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    ) -> Self {
        Self::Api {
            request: ChainApiRequest::GetBlockEvents {
                header_id,
                response_tx,
            },
        }
    }

    #[must_use]
    pub const fn remove_block_request(
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    ) -> Self {
        Self::Api {
            request: ChainApiRequest::RemoveBlock {
                header_id,
                response_tx,
            },
        }
    }

    #[must_use]
    pub const fn store_immutable_block_ids_request(
        ids: BTreeMap<Slot, HeaderId>,
        response_tx: Sender<Result<(), String>>,
    ) -> Self {
        Self::Api {
            request: ChainApiRequest::StoreImmutableBlockIds { ids, response_tx },
        }
    }

    #[must_use]
    pub const fn get_immutable_block_id_request(
        slot: Slot,
        response_tx: Sender<Option<HeaderId>>,
    ) -> Self {
        Self::Api {
            request: ChainApiRequest::GetImmutableBlockId { slot, response_tx },
        }
    }

    #[must_use]
    pub const fn scan_immutable_block_ids_request(
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
        response_tx: Sender<Vec<HeaderId>>,
    ) -> Self {
        Self::Api {
            request: ChainApiRequest::ScanImmutableBlockIds {
                slot_range,
                limit,
                response_tx,
            },
        }
    }

    #[must_use]
    pub const fn store_transactions_request(transactions: HashMap<TxHash, Bytes>) -> Self {
        Self::Api {
            request: ChainApiRequest::StoreTransactions { transactions },
        }
    }

    #[must_use]
    pub const fn get_transactions_request(
        tx_hashes: Vec<TxHash>,
        response_tx: Sender<Pin<Box<dyn Stream<Item = Bytes> + Send>>>,
    ) -> Self {
        Self::Api {
            request: ChainApiRequest::GetTransactions {
                tx_hashes,
                response_tx,
            },
        }
    }

    #[must_use]
    pub const fn remove_transactions_request(tx_hashes: Vec<TxHash>) -> Self {
        Self::Api {
            request: ChainApiRequest::RemoveTransactions { tx_hashes },
        }
    }
}

#[cfg(feature = "rocksdb-backend")]
async fn handle_store_transactions(
    backend: &mut RocksBackend,
    transactions: HashMap<TxHash, Bytes>,
) -> Result<(), StorageServiceError> {
    backend
        .store_transactions(transactions)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    Ok(())
}

#[cfg(feature = "rocksdb-backend")]
fn handle_get_transactions(
    backend: &RocksBackend,
    tx_hashes: Vec<TxHash>,
    response_tx: Sender<Pin<Box<dyn Stream<Item = Bytes> + Send>>>,
) -> Result<(), StorageServiceError> {
    let result = backend.get_transactions(tx_hashes);

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for get transactions batch request".to_owned(),
        });
    }

    Ok(())
}

#[cfg(feature = "rocksdb-backend")]
async fn handle_remove_transactions(
    backend: &mut RocksBackend,
    tx_hashes: Vec<TxHash>,
) -> Result<(), StorageServiceError> {
    backend
        .remove_transactions(&tx_hashes)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    Ok(())
}
