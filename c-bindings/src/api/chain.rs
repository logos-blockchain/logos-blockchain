use std::ffi::{CString, c_char};

use crate::{OperationStatus, errors::OperationStatusCode, result::FfiStatusResult};

/// Result type for [`get_chain_id`]. On success, `value` is a pointer to a
/// NUL-terminated C string holding the chain ID.
pub type FfiGetChainIdResult = FfiStatusResult<*mut c_char>;

/// Returns the chain ID of the deployment this process was started with.
///
/// The chain ID is fixed by the node's deployment settings and never changes
/// while the process runs, so this reads a process-wide value recorded at
/// startup rather than querying a running service. A node must have been
/// started with [`start_lb_node`](super::lifecycle::start_lb_node) before this
/// call; otherwise it fails with
/// [`OperationStatusCode::NotFound`](OperationStatusCode::NotFound).
///
/// # Returns
///
/// A [`FfiGetChainIdResult`] containing a pointer to an allocated C string on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn get_chain_id() -> FfiGetChainIdResult {
    let Some(chain_id) = lb_node::config::deployment::chain_id() else {
        return FfiGetChainIdResult::err(OperationStatus::error(
            OperationStatusCode::NotFound,
            "Chain ID is not available: no node has been started in this process.",
        ));
    };

    match CString::new(<_ as AsRef<str>>::as_ref(chain_id)) {
        Ok(chain_id) => FfiGetChainIdResult::ok(chain_id.into_raw()),
        Err(error) => FfiGetChainIdResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )),
    }
}
