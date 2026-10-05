use std::{
    collections::{BTreeMap, HashMap},
    num::NonZeroUsize,
    ops::RangeInclusive,
    pin::Pin,
};

use bytes::Bytes;
use futures::{Stream, TryStreamExt as _};
use lb_core::{header::HeaderId, mantle::TxHash};
use lb_cryptarchia_engine::Slot;
use tokio::sync::oneshot::Sender;

use super::RocksBackend;
use crate::{
    StorageMsg, StorageServiceError,
    backend::{StorageBackend, StorageTransaction},
};

pub async fn handle_request(
    msg: StorageMsg,
    backend: &mut RocksBackend,
) -> Result<(), StorageServiceError> {
    match msg {
        StorageMsg::Load { key, reply_channel } => {
            return handle_load(backend, key, reply_channel).await;
        }
        StorageMsg::LoadPrefix {
            prefix,
            start_key,
            end_key,
            limit,
            reply_channel,
        } => {
            return handle_load_prefix(backend, prefix, start_key, end_key, limit, reply_channel)
                .await;
        }
        StorageMsg::Store { key, value } => return handle_store(backend, key, value).await,
        StorageMsg::Remove { key, reply_channel } => {
            return handle_remove(backend, key, reply_channel).await;
        }
        StorageMsg::Execute {
            transaction,
            reply_channel,
        } => return handle_execute(backend, transaction, reply_channel).await,

        StorageMsg::GetBlock {
            header_id,
            response_tx,
        } => handle_get_block(backend, header_id, response_tx).await,

        StorageMsg::StoreBlockData {
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

        StorageMsg::RemoveBlock {
            header_id,
            response_tx,
        } => handle_remove_block(backend, header_id, response_tx).await,

        StorageMsg::GetBlockParent {
            header_id,
            response_tx,
        } => handle_get_block_parent(backend, header_id, response_tx).await,

        StorageMsg::GetBlockEvents {
            header_id,
            response_tx,
        } => handle_get_block_events(backend, header_id, response_tx).await,

        StorageMsg::StoreImmutableBlockIds {
            ids: block_ids,
            response_tx,
        } => handle_store_immutable_block_ids(backend, block_ids, response_tx).await,

        StorageMsg::GetImmutableBlockId { slot, response_tx } => {
            handle_get_immutable_block_id(backend, slot, response_tx).await
        }

        StorageMsg::ScanImmutableBlockIds {
            slot_range,
            limit,
            response_tx,
        } => handle_scan_immutable_block_ids(backend, slot_range, limit, response_tx).await,

        StorageMsg::ScanImmutableBlockIdsReverse {
            slot_range,
            limit,
            response_tx,
        } => handle_scan_immutable_block_ids_reverse(backend, slot_range, limit, response_tx).await,

        StorageMsg::StoreTransactions { transactions } => {
            handle_store_transactions(backend, transactions).await
        }

        StorageMsg::GetTransactions {
            tx_hashes,
            response_tx,
        } => handle_get_transactions(backend, tx_hashes, response_tx),

        StorageMsg::RemoveTransactions { tx_hashes } => {
            handle_remove_transactions(backend, tx_hashes).await
        }
    }
    .map_err(|error| StorageServiceError::BackendError(error.into()))
}

/// Handle load message
async fn handle_load(
    backend: &mut RocksBackend,
    key: Bytes,
    reply_channel: Sender<Option<Bytes>>,
) -> Result<(), StorageServiceError> {
    let result: Option<Bytes> = backend
        .load(&key)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    reply_channel
        .send(result)
        .map_err(|_| StorageServiceError::ReplyError {
            message: format!("Load {key:?}"),
        })
}

/// Handle load prefix message
async fn handle_load_prefix(
    backend: &mut RocksBackend,
    prefix: Bytes,
    start_key: Option<Bytes>,
    end_key: Option<Bytes>,
    limit: Option<NonZeroUsize>,
    reply_channel: Sender<Vec<Bytes>>,
) -> Result<(), StorageServiceError> {
    let result: Vec<Bytes> = backend
        .load_prefix(&prefix, start_key.as_deref(), end_key.as_deref(), limit)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    reply_channel
        .send(result)
        .map_err(|_| StorageServiceError::ReplyError {
            message: format!("LoadPrefix {prefix:?}"),
        })
}

/// Handle remove message
async fn handle_remove(
    backend: &mut RocksBackend,
    key: Bytes,
    reply_channel: Sender<Option<Bytes>>,
) -> Result<(), StorageServiceError> {
    let result: Option<Bytes> = backend
        .remove(&key)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    reply_channel
        .send(result)
        .map_err(|_| StorageServiceError::ReplyError {
            message: format!("Remove {key:?}"),
        })
}

/// Handle store message
async fn handle_store(
    backend: &mut RocksBackend,
    key: Bytes,
    value: Bytes,
) -> Result<(), StorageServiceError> {
    backend
        .store(key, value)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))
}

/// Handle execute message
async fn handle_execute(
    backend: &mut RocksBackend,
    transaction: <RocksBackend as StorageBackend>::Transaction,
    reply_channel: Sender<
        <<RocksBackend as StorageBackend>::Transaction as StorageTransaction>::Result,
    >,
) -> Result<(), StorageServiceError> {
    let result = backend
        .execute(transaction)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    reply_channel
        .send(result)
        .map_err(|_| StorageServiceError::ReplyError {
            message: "Execute transaction".to_owned(),
        })
}

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

/// Helper to collect a stream of immutable `HeaderId`s into a reversed `Vec`.
pub(super) async fn streamed_immutable_block_ids_reverse_vec(
    backend: &mut RocksBackend,
    slot_range: RangeInclusive<Slot>,
    limit: NonZeroUsize,
) -> Result<Vec<HeaderId>, StorageServiceError> {
    let stream = backend
        .scan_immutable_block_ids_reverse(slot_range, limit)
        .await
        .map_err(|e| StorageServiceError::BackendError(Box::new(e)))?;
    stream
        .try_collect::<Vec<HeaderId>>()
        .await
        .map_err(StorageServiceError::BackendError)
}

/// Helper to collect a stream of immutable `HeaderId`s into a `Vec`.
pub(super) async fn streamed_immutable_block_ids_vec(
    backend: &mut RocksBackend,
    slot_range: RangeInclusive<Slot>,
    limit: NonZeroUsize,
) -> Result<Vec<HeaderId>, StorageServiceError> {
    let stream = backend
        .scan_immutable_block_ids(slot_range, limit)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    stream
        .try_collect::<Vec<HeaderId>>()
        .await
        .map_err(StorageServiceError::BackendError)
}
