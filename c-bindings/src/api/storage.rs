use std::ffi::{CString, c_char};

use lb_node::{RocksBackend, RuntimeServiceId, SignedMantleTx};

use crate::{
    LogosBlockchainNode, PointerResult,
    api::cryptarchia::{HeaderId, TxHash, into_tx_hash},
    errors::OperationStatus,
};

/// Gets a block by its header ID as a JSON string.
///
/// This is a synchronous wrapper around the asynchronous
/// [`get_block`](lb_api_service::http::mantle::get_block) function.
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
/// [`OperationStatus::NotFound`] if no block with the given header ID exists.
pub(crate) fn get_block_sync(
    node: &LogosBlockchainNode,
    header_id: HeaderId,
) -> Result<CString, OperationStatus> {
    let runtime_handle = node.get_runtime_handle();
    let overwatch_handle = node.get_overwatch_handle();

    let block = runtime_handle
        .block_on(lb_api_service::http::mantle::get_block::<
            SignedMantleTx,
            RocksBackend,
            RuntimeServiceId,
        >(
            overwatch_handle,
            lb_core::header::HeaderId::from(header_id),
        ))
        .map_err(|e| {
            log::error!("[get_block_sync] Failed to get block: {e}");
            OperationStatus::RelayError
        })?
        .ok_or(OperationStatus::NotFound)?;

    let json = serde_json::to_string(&block).map_err(|e| {
        log::error!("[get_block_sync] Failed to serialize block: {e}");
        OperationStatus::RuntimeError
    })?;

    CString::new(json).map_err(|e| {
        log::error!("[get_block_sync] Failed to create CString: {e}");
        OperationStatus::RuntimeError
    })
}

/// Result type for `get_block`. On success, `value` is a pointer to a
/// NUL-terminated C string containing the JSON-serialized block.
pub type GetBlockResult = PointerResult<c_char, OperationStatus>;

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
/// A [`GetBlockResult`] containing a pointer to an allocated C string (JSON
/// block) on success, or an [`OperationStatus`] error on failure. Returns
/// [`OperationStatus::NotFound`] if no block with the given header ID exists.
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_block(
    node: *const LogosBlockchainNode,
    header_id: *const HeaderId,
) -> GetBlockResult {
    if node.is_null() {
        log::error!("[get_block] Received a null `node` pointer. Exiting.");
        return GetBlockResult::from_error(OperationStatus::NullPointer);
    }
    if header_id.is_null() {
        log::error!("[get_block] Received a null `header_id` pointer. Exiting.");
        return GetBlockResult::from_error(OperationStatus::NullPointer);
    }

    let header_id = unsafe { *header_id };
    let node = unsafe { &*node };
    match get_block_sync(node, header_id) {
        Ok(json_cstring) => GetBlockResult::from_pointer(json_cstring.into_raw()),
        Err(error) => GetBlockResult::from_error(error),
    }
}

/// Gets a transaction by its hash as a JSON string.
///
/// This is a synchronous wrapper around the asynchronous
/// [`get_transaction`](lb_api_service::http::mantle::get_transaction) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tx_hash`: The [`lb_core::mantle::TxHash`] of the transaction to fetch.
///
/// # Returns
///
/// A `Result` containing a JSON string of the transaction on success, or an
/// [`OperationStatus`] error on failure. Returns [`OperationStatus::NotFound`]
/// if no transaction with the given hash exists.
pub(crate) fn get_transaction_sync(
    node: &LogosBlockchainNode,
    tx_hash: lb_core::mantle::TxHash,
) -> Result<CString, OperationStatus> {
    let runtime_handle = node.get_runtime_handle();
    let overwatch_handle = node.get_overwatch_handle();

    let tx = runtime_handle
        .block_on(lb_api_service::http::mantle::get_transaction::<
            SignedMantleTx,
            RocksBackend,
            RuntimeServiceId,
        >(overwatch_handle, tx_hash))
        .map_err(|e| {
            log::error!("[get_transaction_sync] Failed to get transaction: {e}");
            OperationStatus::RuntimeError
        })?
        .ok_or(OperationStatus::NotFound)?;

    let json = serde_json::to_string(&tx).map_err(|e| {
        log::error!("[get_transaction_sync] Failed to serialize transaction: {e}");
        OperationStatus::RuntimeError
    })?;

    CString::new(json).map_err(|e| {
        log::error!("[get_transaction_sync] Failed to create CString: {e}");
        OperationStatus::RuntimeError
    })
}

/// Result type for `get_transaction`. On success, `value` is a pointer to a
/// NUL-terminated C string containing the JSON-serialized transaction.
pub type GetTransactionResult = PointerResult<c_char, OperationStatus>;

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
/// A [`GetTransactionResult`] containing a pointer to an allocated C string
/// (JSON transaction) on success, or an [`OperationStatus`] error on failure.
/// Returns [`OperationStatus::NotFound`] if no transaction with the given hash
/// exists.
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_transaction(
    node: *const LogosBlockchainNode,
    tx_hash: *const TxHash,
) -> GetTransactionResult {
    if node.is_null() {
        log::error!("[get_transaction] Received a null `node` pointer. Exiting.");
        return GetTransactionResult::from_error(OperationStatus::NullPointer);
    }
    if tx_hash.is_null() {
        log::error!("[get_transaction] Received a null `tx_hash` pointer. Exiting.");
        return GetTransactionResult::from_error(OperationStatus::NullPointer);
    }

    let node = unsafe { &*node };
    let tx_hash_result = unsafe { into_tx_hash(tx_hash) };
    let tx_hash = match tx_hash_result {
        Ok(tx_hash) => tx_hash,
        Err(error) => {
            log::error!("[get_transaction] Invalid `tx_hash`. Exiting.");
            return GetTransactionResult::from_error(error);
        }
    };

    match get_transaction_sync(node, tx_hash) {
        Ok(json_cstring) => GetTransactionResult::from_pointer(json_cstring.into_raw()),
        Err(error) => GetTransactionResult::from_error(error),
    }
}

/// Gets blocks in a slot range as a JSON array string.
///
/// This is a synchronous wrapper around the asynchronous
/// [`get_blocks`](lb_api_service::http::mantle::get_blocks) function.
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
) -> Result<CString, OperationStatus> {
    let runtime_handle = node.get_runtime_handle();
    let overwatch_handle = node.get_overwatch_handle();

    let blocks = runtime_handle
        .block_on(lb_api_service::http::mantle::get_blocks::<
            SignedMantleTx,
            RocksBackend,
            RuntimeServiceId,
        >(overwatch_handle, from_slot, to_slot))
        .map_err(|e| {
            log::error!("[get_blocks_sync] Failed to get blocks: {e}");
            OperationStatus::RelayError
        })?;

    let json = serde_json::to_string(&blocks).map_err(|e| {
        log::error!("[get_blocks_sync] Failed to serialize blocks: {e}");
        OperationStatus::RuntimeError
    })?;

    CString::new(json).map_err(|e| {
        log::error!("[get_blocks_sync] Failed to create CString: {e}");
        OperationStatus::RuntimeError
    })
}

/// Result type for `get_blocks`. On success, `value` is a pointer to a
/// NUL-terminated C string containing a JSON array of blocks.
pub type GetBlocksResult = PointerResult<c_char, OperationStatus>;

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
/// A [`GetBlocksResult`] containing a pointer to an allocated C string (JSON
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_blocks(
    node: *const LogosBlockchainNode,
    from_slot: u64,
    to_slot: u64,
) -> GetBlocksResult {
    if node.is_null() {
        log::error!("[get_blocks] Received a null `node` pointer. Exiting.");
        return GetBlocksResult::from_error(OperationStatus::NullPointer);
    }

    let Ok(from_slot) = usize::try_from(from_slot) else {
        log::error!("[get_blocks] from_slot overflow");
        return GetBlocksResult::from_error(OperationStatus::ValidationError);
    };
    let Ok(to_slot) = usize::try_from(to_slot) else {
        log::error!("[get_blocks] to_slot overflow");
        return GetBlocksResult::from_error(OperationStatus::ValidationError);
    };

    let node = unsafe { &*node };
    match get_blocks_sync(node, from_slot, to_slot) {
        Ok(json_cstring) => GetBlocksResult::from_pointer(json_cstring.into_raw()),
        Err(error) => GetBlocksResult::from_error(error),
    }
}
