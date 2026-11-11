use std::ffi::{CString, c_char};

use futures::StreamExt as _;
use lb_c_macros::panic_to_error;
use lb_core::{
    block::Block as CoreBlock,
    mantle::{
        ledger::verification_mode::StandardMode,
        traits::{Hashable, StorageSize, hashable},
        transactions::{hash::TxHash as CoreTxHash, states::Unverified},
    },
};
use lb_node::{SignedOps, api::serializers::blocks::ApiProcessedBlockEventOwned};
use serde::Serialize;

use crate::{
    LogosBlockchainNode, OperationStatus,
    api::{
        node::to_json_c_string,
        types::{
            block::Block,
            hash::{HeaderId, TxHash, into_tx_hash},
        },
    },
    callbacks::{BoxedCallback, EventCallback, into_boxed_callback},
    errors::OperationStatusCode,
    logging,
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

#[derive(Serialize)]
#[serde(rename = "SignedMantleTx")]
#[derive(Clone)]
pub struct TxWithId {
    id: CoreTxHash,
    #[serde(flatten)]
    tx: SignedOps<Unverified, StandardMode>,
}

impl Hashable for TxWithId {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: hashable::Hasher<Self> =
        |tx| <SignedOps<Unverified, StandardMode> as Hashable>::HASHER(&tx.tx);
    type Hash = <SignedOps<Unverified, StandardMode> as Hashable>::Hash;

    fn as_signing(&self) -> Vec<u8> {
        self.tx.as_signing()
    }
}

impl StorageSize for TxWithId {
    fn storage_size(&self) -> usize {
        self.tx.storage_size()
    }
}

#[must_use]
pub fn subscribe_to_new_blocks_sync(
    node: &LogosBlockchainNode,
    mut callback_per_block: BoxedCallback<*const c_char>,
) -> OperationStatus {
    let runtime_handler = unwrap_or_return_error!(node.get_runtime_handle());
    let overwatch = node.get_overwatch_handle();
    runtime_handler.block_on(async move {
        let stream = match lb_node::api::calls::blocks::get_new_blocks_stream(overwatch).await {
            Ok(stream) => stream,
            Err(e) => {
                return OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to subscribe to blocks: {e}"),
                );
            }
        };
        runtime_handler.spawn(async move {
            let mut stream = Box::pin(stream);
            while let Some(event) = stream.next().await {
                let block = event.block;
                let txs_with_id = block.transactions().map_ref(|tx| TxWithId {
                    id: tx.hash(),
                    tx: tx.clone(),
                });
                let block: CoreBlock<TxWithId> = CoreBlock::reconstruct(
                    block.header().clone(),
                    block.uncle_headers().clone(),
                    txs_with_id,
                    *block.signature(),
                    u32::MAX,
                )
                .expect("Block should always build from valid block");
                callback_per_block(Block::from(block).as_ptr());
            }
            logging::warning!(
                "subscribe_to_new_blocks_sync",
                "Block stream closed, subscription to new blocks ended."
            );
            callback_per_block(std::ptr::null());
        });
        OperationStatus::OK
    })
}

/// Subscribes to new blocks on the blockchain and calls the provided callback
/// for each new block.
///
/// Deprecated: prefer [`subscribe_to_processed_blocks`], which additionally
/// carries the chain state per event (tip, tip slot, LIB, LIB slot) and the
/// block header id, uses the same JSON schema as the node's
/// `/cryptarchia/blocks/stream` HTTP endpoint (transaction ids at
/// `transactions[].mantle_tx.hash`), and signals the end of the stream. This
/// call will be removed once consumers have migrated.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `callback_per_block`: A callback function that will be called with a
///   pointer to a C string containing the JSON representation of each new
///   block. When the stream ends (e.g. the consumer falls behind or the node
///   shuts down) the callback is called exactly once with NULL, after which no
///   further blocks are delivered.
///
/// # Returns
///
/// An [`OperationStatus`] indicating success or failure.
///
/// A null callback is accepted: the call then subscribes to nothing and
/// returns success.
///
/// The callback runs on one of the node's own threads. It must not call back
/// into the node: every function taking a [`LogosBlockchainNode`] fails with
/// [`OperationStatusCode::RuntimeError`] when called from inside a callback.
/// Hand the event to another thread instead.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn subscribe_to_new_blocks(
    node: *const LogosBlockchainNode,
    callback_per_block: EventCallback,
) -> OperationStatus {
    return_error_if_null_pointer!(node);
    // Without a callback there is nobody to deliver events to, so nothing is
    // subscribed.
    let Some(callback_per_block) = callback_per_block else {
        return OperationStatus::OK;
    };
    let node = unsafe { &*node };
    let callback_per_block = unsafe { into_boxed_callback(callback_per_block) };
    subscribe_to_new_blocks_sync(node, callback_per_block)
}

/// Serializes `value` as JSON and invokes `on_event` with a pointer to the
/// NUL-terminated string. The pointer is only valid for the duration of the
/// callback invocation.
fn emit_json<T: Serialize>(value: &T, on_event: &mut BoxedCallback<*const c_char>) {
    let json = CString::new(
        serde_json::to_string(value).expect("Serialization of an event should always succeed"),
    )
    .expect("Event JSON should not contain NUL bytes");
    on_event(json.as_ptr());
}

#[must_use]
pub fn subscribe_to_processed_blocks_sync(
    node: &LogosBlockchainNode,
    mut on_event: BoxedCallback<*const c_char>,
) -> OperationStatus {
    let runtime_handler = unwrap_or_return_error!(node.get_runtime_handle());
    let overwatch = node.get_overwatch_handle();
    runtime_handler.block_on(async move {
        let stream = match lb_node::api::calls::blocks::get_new_blocks_stream(overwatch).await {
            Ok(stream) => stream,
            Err(e) => {
                return OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to subscribe to processed blocks: {e}"),
                );
            }
        };
        runtime_handler.spawn(async move {
            let mut stream = Box::pin(stream);
            while let Some(event) = stream.next().await {
                emit_json(&ApiProcessedBlockEventOwned::from(event), &mut on_event);
            }
            on_event(std::ptr::null());
        });
        OperationStatus::OK
    })
}

/// Subscribes to processed block events and calls `callback_per_event` for
/// each one.
///
/// Each event carries the full block along with the chain state after
/// processing it (tip, tip slot, LIB, LIB slot), serialized as JSON with the
/// same schema as the node's `/cryptarchia/blocks/stream` HTTP endpoint.
/// Transaction ids are at `transactions[].mantle_tx.hash`.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `callback_per_event`: Called with a pointer to a NUL-terminated JSON event
///   per processed block. The pointer is only valid for the duration of the
///   call — copy the data if it is needed longer. When the stream ends (e.g. on
///   node shutdown) the callback is called exactly once with NULL, after which
///   no further events are delivered; re-subscribe to keep receiving events.
///   Declared unsafe extern "C"; must be thread-safe.
///
/// # Returns
///
/// An [`OperationStatus`] indicating whether the subscription was established.
/// On error, the callback is never called.
///
/// A null callback is accepted: the call then subscribes to nothing and
/// returns success.
///
/// The callback runs on one of the node's own threads. It must not call back
/// into the node: every function taking a [`LogosBlockchainNode`] fails with
/// [`OperationStatusCode::RuntimeError`] when called from inside a callback.
/// Hand the event to another thread instead.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn subscribe_to_processed_blocks(
    node: *const LogosBlockchainNode,
    callback_per_event: EventCallback,
) -> OperationStatus {
    return_error_if_null_pointer!(node);
    // Without a callback there is nobody to deliver events to, so nothing is
    // subscribed.
    let Some(callback_per_event) = callback_per_event else {
        return OperationStatus::OK;
    };
    let node = unsafe { &*node };
    subscribe_to_processed_blocks_sync(node, unsafe { into_boxed_callback(callback_per_event) })
}

#[must_use]
pub fn subscribe_to_lib_blocks_sync(
    node: &LogosBlockchainNode,
    mut on_event: BoxedCallback<*const c_char>,
) -> OperationStatus {
    let runtime_handler = unwrap_or_return_error!(node.get_runtime_handle());
    let overwatch = node.get_overwatch_handle();
    runtime_handler.block_on(async move {
        let stream = match lb_node::api::calls::blocks::lib_block_stream(overwatch).await {
            Ok(stream) => stream,
            Err(e) => {
                return OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to subscribe to LIB blocks: {e}"),
                );
            }
        };
        runtime_handler.spawn(async move {
            let mut stream = Box::pin(stream);
            while let Some(item) = stream.next().await {
                match item {
                    Ok(block_info) => {
                        emit_json(&block_info, &mut on_event);
                    }
                    Err(e) => {
                        logging::error!(
                            "subscribe_to_lib_blocks_sync",
                            "LIB block stream failed: {e}"
                        );
                        break;
                    }
                }
            }
            on_event(std::ptr::null());
        });
        OperationStatus::OK
    })
}

/// Subscribes to Last Irreversible Block (LIB) updates and calls
/// `callback_per_event` for each newly finalized block.
///
/// Each event is the finalized block's info serialized as JSON with the same
/// schema as the node's `/cryptarchia/lib/stream` HTTP endpoint.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
/// - `callback_per_event`: Called with a pointer to a NUL-terminated JSON event
///   per finalized block. The pointer is only valid for the duration of the
///   call — copy the data if it is needed longer. When the stream ends or fails
///   the callback is called exactly once with NULL, after which no further
///   events are delivered; re-subscribe to keep receiving events. Declared
///   unsafe extern "C"; must be thread-safe.
///
/// # Returns
///
/// An [`OperationStatus`] indicating whether the subscription was established.
/// On error, the callback is never called.
///
/// A null callback is accepted: the call then subscribes to nothing and
/// returns success.
///
/// The callback runs on one of the node's own threads. It must not call back
/// into the node: every function taking a [`LogosBlockchainNode`] fails with
/// [`OperationStatusCode::RuntimeError`] when called from inside a callback.
/// Hand the event to another thread instead.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn subscribe_to_lib_blocks(
    node: *const LogosBlockchainNode,
    callback_per_event: EventCallback,
) -> OperationStatus {
    return_error_if_null_pointer!(node);
    // Without a callback there is nobody to deliver events to, so nothing is
    // subscribed.
    let Some(callback_per_event) = callback_per_event else {
        return OperationStatus::OK;
    };
    let node = unsafe { &*node };
    subscribe_to_lib_blocks_sync(node, unsafe { into_boxed_callback(callback_per_event) })
}

/// Gets a block by its header ID as a JSON string.
///
/// This is a synchronous wrapper around the asynchronous
/// [`get_block`](lb_node::api::calls::blocks::get_block) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `header_id`: The 32-byte header ID of the block to fetch.
///
/// # Returns
///
/// A `Result` containing a JSON string representation of `Block` on success,
/// or an [`OperationStatus`] error on failure. Returns
/// [`OperationStatusCode::NotFound`] if no block with the given header ID
/// exists.
pub(crate) fn get_block_sync(
    node: &LogosBlockchainNode,
    header_id: HeaderId,
) -> StatusResult<CString> {
    let block = node.call(
        "Failed to get block",
        lb_node::api::calls::blocks::get_block(
            node.get_overwatch_handle(),
            lb_core::header::HeaderId::from(header_id),
        ),
    )?;
    to_json_c_string(&block)
}

/// Result type for `get_block`. On success, `value` is a pointer to a
/// NUL-terminated C string containing the JSON-serialized block.
pub type FfiGetBlockResult = FfiStatusResult<*mut c_char>;

/// Get a block by its header ID as a JSON string.
///
/// Returns the JSON-serialized block for the given 32-byte header ID.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`].
/// - `header_id`: A non-null pointer to a 32-byte header ID.
///
/// # Returns
///
/// A [`FfiGetBlockResult`] containing a pointer to an allocated C string (JSON
/// block) on success, or an [`OperationStatus`] error on failure. Returns
/// [`OperationStatusCode::NotFound`] if no block with the given header ID
/// exists.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`], and that `header_id` is non-null and points to at
/// least 32 valid bytes.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_block(
    node: *const LogosBlockchainNode,
    header_id: *const HeaderId,
) -> FfiGetBlockResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(header_id);

    let header_id = unsafe { *header_id };
    let node = unsafe { &*node };
    let json_cstring = unwrap_or_return_error!(get_block_sync(node, header_id));
    FfiGetBlockResult::ok(json_cstring.into_raw())
}

/// Gets a transaction by its hash as a JSON string.
///
/// This is a synchronous wrapper around the asynchronous
/// [`get_transaction`](lb_node::api::calls::blocks::get_transaction) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tx_hash`: The [`lb_core::mantle::TxHash`] of the transaction to fetch.
///
/// # Returns
///
/// A `Result` containing a JSON string of the transaction on success, or an
/// [`OperationStatus`] error on failure. Returns
/// [`OperationStatusCode::NotFound`] if no transaction with the given hash
/// exists.
pub(crate) fn get_transaction_sync(
    node: &LogosBlockchainNode,
    tx_hash: lb_core::mantle::TxHash,
) -> StatusResult<CString> {
    let transaction = node.call(
        "Failed to get transaction",
        lb_node::api::calls::blocks::get_transaction(node.get_overwatch_handle(), tx_hash),
    )?;
    to_json_c_string(&transaction)
}

/// Result type for `get_transaction`. On success, `value` is a pointer to a
/// NUL-terminated C string containing the JSON-serialized transaction.
pub type FfiGetTransactionResult = FfiStatusResult<*mut c_char>;

/// Get a transaction by its hash as a JSON string.
///
/// Returns the JSON-serialized transaction for the given 32-byte transaction
/// hash.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`].
/// - `tx_hash`: A non-null pointer to the 32-byte transaction hash.
///
/// # Returns
///
/// A [`FfiGetTransactionResult`] containing a pointer to an allocated C string
/// (JSON transaction) on success, or an [`OperationStatus`] error on failure.
/// Returns [`OperationStatusCode::NotFound`] if no transaction with the given
/// hash exists.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`], and that `tx_hash` is non-null and points to at
/// least 32 valid bytes.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_transaction(
    node: *const LogosBlockchainNode,
    tx_hash: *const TxHash,
) -> FfiGetTransactionResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(tx_hash);

    let node = unsafe { &*node };
    let tx_hash = unsafe { into_tx_hash(tx_hash) };
    let json_cstring = unwrap_or_return_error!(get_transaction_sync(node, tx_hash));
    FfiGetTransactionResult::ok(json_cstring.into_raw())
}

/// Gets blocks in a slot range as a JSON array string.
///
/// This is a synchronous wrapper around the asynchronous
/// [`get_blocks`](lb_node::api::calls::blocks::get_immutable_blocks) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `from_slot`: Starting slot (inclusive).
/// - `to_slot`: Ending slot (inclusive).
///
/// # Returns
///
/// A `Result` containing a JSON string representation of `Vec<Block>` on
/// success, or an [`OperationStatus`] error on failure.
pub(crate) fn get_blocks_sync(
    node: &LogosBlockchainNode,
    from_slot: usize,
    to_slot: usize,
) -> StatusResult<CString> {
    let blocks = node.call(
        "Failed to get blocks",
        lb_node::api::calls::blocks::get_immutable_blocks(
            node.get_overwatch_handle(),
            from_slot,
            to_slot,
        ),
    )?;
    to_json_c_string(&blocks)
}

/// Result type for `get_blocks`. On success, `value` is a pointer to a
/// NUL-terminated C string containing a JSON array of blocks.
pub type FfiGetBlocksResult = FfiStatusResult<*mut c_char>;

/// Get blocks in a slot range as a JSON array string.
///
/// Returns a JSON array of blocks for the specified slot range.
/// The JSON format matches the server's block serialization.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`].
/// - `from_slot`: Starting slot (inclusive).
/// - `to_slot`: Ending slot (inclusive).
///
/// # Returns
///
/// A [`FfiGetBlocksResult`] containing a pointer to an allocated C string (JSON
/// array) on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that all pointers are non-null and point to valid
/// memory.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_blocks(
    node: *const LogosBlockchainNode,
    from_slot: u64,
    to_slot: u64,
) -> FfiGetBlocksResult {
    return_error_if_null_pointer!(node);

    let Ok(from_slot) = usize::try_from(from_slot) else {
        return FfiGetBlocksResult::err(OperationStatus::error(
            OperationStatusCode::ValidationError,
            "from_slot overflow.",
        ));
    };
    let Ok(to_slot) = usize::try_from(to_slot) else {
        return FfiGetBlocksResult::err(OperationStatus::error(
            OperationStatusCode::ValidationError,
            "to_slot overflow.",
        ));
    };

    let node = unsafe { &*node };
    let json_cstring = unwrap_or_return_error!(get_blocks_sync(node, from_slot, to_slot));
    FfiGetBlocksResult::ok(json_cstring.into_raw())
}

/// Gets a block's events as a JSON string.
///
/// This is a synchronous wrapper around the asynchronous
/// [`CryptarchiaServiceApi::get_block_events`] function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `header_id`: The 32-byte header ID of the block whose events to fetch.
///
/// # Returns
///
/// A `Result` containing a JSON string of the block's events on success, or an
/// [`OperationStatus`] error on failure. Returns
/// [`OperationStatusCode::NotFound`] if no block with the given header ID
/// exists.
pub(crate) fn get_block_events_sync(
    node: &LogosBlockchainNode,
    header_id: HeaderId,
) -> StatusResult<CString> {
    let events = node.call(
        "Failed to get block events",
        lb_node::api::calls::blocks::get_block_events(
            node.get_overwatch_handle(),
            lb_core::header::HeaderId::from(header_id),
        ),
    )?;
    to_json_c_string(&events)
}

/// Result type for `get_block_events`. On success, `value` is a pointer to a
/// NUL-terminated C string containing the JSON-serialized events.
pub type FfiGetBlockEventsResult = FfiStatusResult<*mut c_char>;

/// Get a block's events as a JSON string.
///
/// Returns the JSON-serialized events emitted by the block with the given
/// 32-byte header ID.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`].
/// - `header_id`: A non-null pointer to a 32-byte header ID.
///
/// # Returns
///
/// A [`FfiGetBlockEventsResult`] containing a pointer to an allocated C string
/// (JSON events) on success, or an [`OperationStatus`] error on failure.
/// Returns [`OperationStatusCode::NotFound`] if no block with the given header
/// ID exists.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`], and that `header_id` is non-null and points to at
/// least 32 valid bytes.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_block_events(
    node: *const LogosBlockchainNode,
    header_id: *const HeaderId,
) -> FfiGetBlockEventsResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(header_id);

    let header_id = unsafe { *header_id };
    let node = unsafe { &*node };
    let json_cstring = unwrap_or_return_error!(get_block_events_sync(node, header_id));
    FfiGetBlockEventsResult::ok(json_cstring.into_raw())
}
