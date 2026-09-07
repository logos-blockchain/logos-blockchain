use std::ffi::{CString, c_char};

use crate::{
    LogosBlockchainNode, OperationStatus, errors::OperationStatusCode, result::FfiStatusResult,
    return_error_if_null_pointer,
};

/// Result type for [`get_chain_id`]. On success, `value` is a pointer to a
/// NUL-terminated C string holding the chain ID.
pub type FfiGetChainIdResult = FfiStatusResult<*mut c_char>;

/// Returns the chain ID of the deployment the given node was started with.
///
/// The chain ID is fixed by the node's deployment settings and never changes
/// while the node runs, so it is captured on the node handle at startup rather
/// than queried from a running service.
///
/// # Arguments
///
/// - `node`: A pointer to the node returned by
///   [`start_lb_node`](super::lifecycle::start_lb_node).
///
/// # Returns
///
/// A [`FfiGetChainIdResult`] containing a pointer to an allocated C string on
/// success, or an [`OperationStatus`] error on failure. A chain ID that cannot
/// be represented as a C string — one carrying an interior NUL byte — fails
/// with [`OperationStatusCode::RuntimeError`].
///
/// # Safety
///
/// `node` must be a valid pointer to a [`LogosBlockchainNode`] that has not
/// been stopped.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_chain_id(node: *const LogosBlockchainNode) -> FfiGetChainIdResult {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };

    let Some(chain_id) = node.chain_id() else {
        return FfiGetChainIdResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            "The chain ID of this deployment cannot be represented as a C string.",
        ));
    };

    // Hand the caller its own copy: the node keeps ownership of its own string
    // for as long as it lives, and the caller frees this one with
    // `free_cstring`.
    match CString::new(chain_id.to_bytes()) {
        Ok(chain_id) => FfiGetChainIdResult::ok(chain_id.into_raw()),
        Err(error) => FfiGetChainIdResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )),
    }
}
