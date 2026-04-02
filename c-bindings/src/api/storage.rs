use std::ffi::{CString, c_char};

use lb_core::block::Block;
use lb_node::{RocksBackend, RuntimeServiceId, SignedMantleTx};

use crate::{LogosBlockchainNode, PointerResult, errors::OperationStatus};

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

    let blocks: Vec<Block<SignedMantleTx>> = runtime_handle
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
