use std::ffi::{CString, c_char};

use lb_core::mantle::ops::channel::ChannelId;
use lb_node::RuntimeServiceId;

use crate::{
    LogosBlockchainNode,
    errors::{OperationStatus, OperationStatusCode},
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

/// Gets a channel's state as a JSON string.
///
/// This is a synchronous wrapper around the asynchronous
/// [`channel_state`](lb_api_service::http::mantle::channel_state) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `channel_id`: The [`ChannelId`] of the channel to fetch.
///
/// # Returns
///
/// A `Result` containing a JSON string of the channel state on success, or an
/// [`OperationStatus`] error on failure. Returns
/// [`OperationStatusCode::NotFound`] if the channel does not exist.
pub(crate) fn get_channel_state_sync(
    node: &LogosBlockchainNode,
    channel_id: ChannelId,
) -> StatusResult<CString> {
    let runtime_handle = node.get_runtime_handle();
    let overwatch_handle = node.get_overwatch_handle();

    let state = runtime_handle
        .block_on(lb_api_service::http::mantle::channel_state::<
            RuntimeServiceId,
        >(overwatch_handle, channel_id))
        .map_err(|e| {
            OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("Failed to get channel state: {e}"),
            )
        })?
        .ok_or_else(|| {
            OperationStatus::error(
                OperationStatusCode::NotFound,
                format!("No channel found for id {channel_id:?}"),
            )
        })?;

    let json = serde_json::to_string(&state).map_err(|e| {
        OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to serialize channel state: {e}"),
        )
    })?;

    CString::new(json).map_err(|e| {
        OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {e}"),
        )
    })
}

/// Result type for `get_channel_state`. On success, `value` is a pointer to a
/// NUL-terminated C string containing the JSON-serialized channel state.
pub type FfiGetChannelStateResult = FfiStatusResult<*mut c_char>;

/// Get a channel's state as a JSON string.
///
/// Returns the JSON-serialized state of the channel with the given 32-byte
/// channel ID, at the current tip.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`].
/// - `channel_id`: A non-null pointer to a 32-byte channel ID.
///
/// # Returns
///
/// A [`FfiGetChannelStateResult`] containing a pointer to an allocated C
/// string (JSON channel state) on success, or an [`OperationStatus`] error on
/// failure. Returns [`OperationStatusCode::NotFound`] if the channel does not
/// exist.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// The caller must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`], and that `channel_id` is non-null and points to at
/// least 32 valid bytes.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_channel_state(
    node: *const LogosBlockchainNode,
    channel_id: *const u8,
) -> FfiGetChannelStateResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(channel_id);

    let channel_id = {
        let bytes = unsafe { std::slice::from_raw_parts(channel_id, 32) };
        let Ok(array): Result<[u8; 32], _> = bytes.try_into() else {
            return FfiGetChannelStateResult::err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                "Invalid channel_id length.",
            ));
        };
        ChannelId::from(array)
    };

    let node = unsafe { &*node };
    let json_cstring = unwrap_or_return_error!(get_channel_state_sync(node, channel_id));
    FfiGetChannelStateResult::ok(json_cstring.into_raw())
}
