use std::ffi::{CString, c_char};

use crate::{
    OperationStatus, api::config::cstr_to_path, errors::OperationStatusCode,
    result::FfiStatusResult, return_error_if_null_pointer,
};

/// Result type for [`get_peer_id`]. On success, `value` is a pointer to a
/// NUL-terminated C string containing the base58-encoded libp2p `PeerId`.
pub type FfiGetPeerIdResult = FfiStatusResult<*mut c_char>;

/// Derives the libp2p `PeerId` from the node key in a user config file,
/// equivalent to the `get-peer-id` CLI command.
///
/// # Arguments
///
/// - `config_path`: Path to the user config YAML file.
///
/// # Returns
///
/// A [`FfiGetPeerIdResult`] containing a pointer to an allocated C string
/// (the base58-encoded `PeerId`) on success, or an [`OperationStatus`] error
/// on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that `config_path` is a valid NUL-terminated C string.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_peer_id(config_path: *const c_char) -> FfiGetPeerIdResult {
    return_error_if_null_pointer!(config_path);

    let config_path = unsafe { cstr_to_path(config_path) };

    let peer_id = match lb_node::cli::get_peer_id::peer_id_from_config(&config_path) {
        Ok(peer_id) => peer_id,
        Err(error) => {
            return FfiGetPeerIdResult::err(OperationStatus::error(
                OperationStatusCode::ConfigurationError,
                format!("Error deriving peer id: {error:?}"),
            ));
        }
    };

    match CString::new(peer_id.to_string()) {
        Ok(peer_id) => FfiGetPeerIdResult::ok(peer_id.into_raw()),
        Err(error) => FfiGetPeerIdResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )),
    }
}
