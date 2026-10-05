use std::ffi::{CString, c_char};

use crate::{OperationStatus, errors::OperationStatusCode, result::FfiStatusResult};

/// Result type for [`get_build_version_info`]. On success, `value` is a pointer
/// to a NUL-terminated C string holding the JSON-encoded version info.
pub type FfiGetBuildVersionInfoResult = FfiStatusResult<*mut c_char>;

/// Returns the version and build provenance of this library, as the same JSON
/// object the node serves over HTTP at `/version`.
///
/// The values are baked in at compile time, so this takes no node handle and is
/// callable any time, even before a node is started.
///
/// # Returns
///
/// A [`FfiGetBuildVersionInfoResult`] containing a pointer to an allocated C
/// string on success, or an [`OperationStatus`] error on failure.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn get_build_version_info() -> FfiGetBuildVersionInfoResult {
    let version_info = lb_version::build_version_info();

    let json = match serde_json::to_string(&version_info) {
        Ok(json) => json,
        Err(error) => {
            return FfiGetBuildVersionInfoResult::err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                format!("Failed to serialize the version info: {error}"),
            ));
        }
    };

    match CString::new(json) {
        Ok(json) => FfiGetBuildVersionInfoResult::ok(json.into_raw()),
        Err(error) => FfiGetBuildVersionInfoResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )),
    }
}
