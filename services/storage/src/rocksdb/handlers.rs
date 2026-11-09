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

use super::RocksBackend;
use crate::{
    StorageMsg, StorageServiceError,
    backend::{StorageBackend, StorageTransaction},
};

pub fn handle_request(msg: StorageMsg, backend: &RocksBackend) -> Result<(), StorageServiceError> {
    match msg {
        StorageMsg::Load { key, reply_channel } => {
            return handle_load(backend, &key, reply_channel);
        }
        StorageMsg::LoadPrefix {
            prefix,
            start_key,
            end_key,
            limit,
            reply_channel,
        } => {
            return handle_load_prefix(
                backend,
                &prefix,
                start_key.as_deref(),
                end_key.as_deref(),
                limit,
                reply_channel,
            );
        }
        StorageMsg::Store { key, value } => return handle_store(backend, key, value),
        StorageMsg::Remove { key, reply_channel } => {
            return handle_remove(backend, &key, reply_channel);
        }
        StorageMsg::Execute {
            transaction,
            reply_channel,
        } => return handle_execute(backend, transaction, reply_channel),

        StorageMsg::GetBlock {
            header_id,
            response_tx,
        } => handle_get_block(backend, header_id, response_tx),

        StorageMsg::StoreBlockData {
            header_id,
            parent_id,
            block,
            events,
            immutable_ids,
            response_tx,
        } => handle_store_block_data(
            backend,
            header_id,
            parent_id,
            block,
            events,
            immutable_ids,
            response_tx,
        ),

        StorageMsg::RemoveBlocks {
            header_ids,
            response_tx,
        } => handle_remove_blocks(backend, &header_ids, response_tx),

        StorageMsg::GetBlockParent {
            header_id,
            response_tx,
        } => handle_get_block_parent(backend, header_id, response_tx),

        StorageMsg::GetBlockEvents {
            header_id,
            response_tx,
        } => handle_get_block_events(backend, header_id, response_tx),

        StorageMsg::StoreImmutableBlockIds {
            ids: block_ids,
            response_tx,
        } => handle_store_immutable_block_ids(backend, block_ids, response_tx),

        StorageMsg::GetImmutableBlockId { slot, response_tx } => {
            handle_get_immutable_block_id(backend, slot, response_tx)
        }

        StorageMsg::ScanImmutableBlockIds {
            slot_range,
            limit,
            response_tx,
        } => handle_scan_immutable_block_ids(backend, slot_range, limit, response_tx),

        StorageMsg::ScanImmutableBlockIdsReverse {
            slot_range,
            limit,
            response_tx,
        } => handle_scan_immutable_block_ids_reverse(backend, slot_range, limit, response_tx),

        StorageMsg::StoreTransactions { transactions } => {
            handle_store_transactions(backend, transactions)
        }

        StorageMsg::GetTransactions {
            tx_hashes,
            response_tx,
        } => handle_get_transactions(backend, tx_hashes, response_tx),

        StorageMsg::RemoveTransactions { tx_hashes } => {
            handle_remove_transactions(backend, &tx_hashes)
        }
    }
    .map_err(|error| StorageServiceError::BackendError(error.into()))
}

/// Handle load message
fn handle_load(
    backend: &RocksBackend,
    key: &Bytes,
    reply_channel: Sender<Option<Bytes>>,
) -> Result<(), StorageServiceError> {
    let result: Option<Bytes> = backend
        .load(key)
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    reply_channel
        .send(result)
        .map_err(|_| StorageServiceError::ReplyError {
            message: format!("Load {key:?}"),
        })
}

/// Handle load prefix message
fn handle_load_prefix(
    backend: &RocksBackend,
    prefix: &Bytes,
    start_key: Option<&[u8]>,
    end_key: Option<&[u8]>,
    limit: Option<NonZeroUsize>,
    reply_channel: Sender<Vec<Bytes>>,
) -> Result<(), StorageServiceError> {
    let result: Vec<Bytes> = backend
        .load_prefix(prefix, start_key, end_key, limit)
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    reply_channel
        .send(result)
        .map_err(|_| StorageServiceError::ReplyError {
            message: format!("LoadPrefix {prefix:?}"),
        })
}

/// Handle remove message
fn handle_remove(
    backend: &RocksBackend,
    key: &Bytes,
    reply_channel: Sender<Option<Bytes>>,
) -> Result<(), StorageServiceError> {
    let result: Option<Bytes> = backend
        .remove(key)
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    reply_channel
        .send(result)
        .map_err(|_| StorageServiceError::ReplyError {
            message: format!("Remove {key:?}"),
        })
}

/// Handle store message
fn handle_store(
    backend: &RocksBackend,
    key: Bytes,
    value: Bytes,
) -> Result<(), StorageServiceError> {
    backend
        .store(key, value)
        .map_err(|e| StorageServiceError::BackendError(e.into()))
}

/// Handle execute message
fn handle_execute(
    backend: &RocksBackend,
    transaction: <RocksBackend as StorageBackend>::Transaction,
    reply_channel: Sender<
        <<RocksBackend as StorageBackend>::Transaction as StorageTransaction>::Result,
    >,
) -> Result<(), StorageServiceError> {
    let result = backend
        .execute(transaction)
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    reply_channel
        .send(result)
        .map_err(|_| StorageServiceError::ReplyError {
            message: "Execute transaction".to_owned(),
        })
}

fn handle_get_block(
    backend: &RocksBackend,
    header_id: HeaderId,
    response_tx: Sender<Option<Bytes>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .get_block(header_id)
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

fn handle_store_block_data(
    backend: &RocksBackend,
    header_id: HeaderId,
    parent_id: HeaderId,
    block: Bytes,
    events: Bytes,
    immutable_ids: BTreeMap<Slot, HeaderId>,
    response_tx: Sender<Result<(), String>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .store_block_data(header_id, parent_id, block, events, immutable_ids)
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

fn handle_get_block_parent(
    backend: &RocksBackend,
    header_id: HeaderId,
    response_tx: Sender<Option<HeaderId>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .get_block_parent(header_id)
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

fn handle_get_block_events(
    backend: &RocksBackend,
    header_id: HeaderId,
    response_tx: Sender<Option<Bytes>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .get_block_events(header_id)
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

fn handle_remove_blocks(
    backend: &RocksBackend,
    header_ids: &[HeaderId],
    response_tx: Sender<Result<(), String>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .remove_blocks(header_ids)
        .map_err(|e| StorageServiceError::BackendError(e.into()));

    let response = result.as_ref().map_err(ToString::to_string).copied();

    response_tx
        .send(response)
        .map_err(|_| StorageServiceError::ReplyError {
            message: "Failed to send reply for remove blocks request".to_owned(),
        })?;

    result
}

fn handle_store_immutable_block_ids(
    backend: &RocksBackend,
    ids: BTreeMap<Slot, HeaderId>,
    response_tx: Sender<Result<(), String>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .store_immutable_block_ids(ids)
        .map_err(|e| StorageServiceError::BackendError(e.into()));

    let response = result.as_ref().map_err(ToString::to_string).copied();

    response_tx
        .send(response)
        .map_err(|_| StorageServiceError::ReplyError {
            message: "Failed to send reply for store immutable block ids request".to_owned(),
        })?;

    result
}

fn handle_get_immutable_block_id(
    backend: &RocksBackend,
    slot: Slot,
    response_tx: Sender<Option<HeaderId>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .get_immutable_block_id(slot)
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

fn handle_scan_immutable_block_ids(
    backend: &RocksBackend,
    slot_range: RangeInclusive<Slot>,
    limit: NonZeroUsize,
    response_tx: Sender<Vec<HeaderId>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .scan_immutable_block_ids(slot_range, limit)
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for scan_immutable_block_ids request".into(),
        });
    }

    Ok(())
}
fn handle_scan_immutable_block_ids_reverse(
    backend: &RocksBackend,
    slot_range: RangeInclusive<Slot>,
    limit: NonZeroUsize,
    response_tx: Sender<Vec<HeaderId>>,
) -> Result<(), StorageServiceError> {
    let result = backend
        .scan_immutable_block_ids_reverse(slot_range, limit)
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;

    if response_tx.send(result).is_err() {
        return Err(StorageServiceError::ReplyError {
            message: "Failed to send reply for scan_immutable_block_ids_reverse request".into(),
        });
    }

    Ok(())
}

fn handle_store_transactions(
    backend: &RocksBackend,
    transactions: HashMap<TxHash, Bytes>,
) -> Result<(), StorageServiceError> {
    backend
        .store_transactions(transactions)
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

fn handle_remove_transactions(
    backend: &RocksBackend,
    tx_hashes: &[TxHash],
) -> Result<(), StorageServiceError> {
    backend
        .remove_transactions(tx_hashes)
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    Ok(())
}
