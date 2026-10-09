use std::ffi::{CString, c_char};

use lb_c_macros::panic_to_error;
use lb_tracing::metrics::open_metrics::read_open_metrics;

use crate::{OperationStatus, errors::OperationStatusCode, result::FfiStatusResult};

/// Result type for [`get_open_metrics`].
/// On success, `value` is a pointer to a NUL-terminated C string holding the
/// rendered `OpenMetrics` document.
pub type FfiGetOpenMetricsResult = FfiStatusResult<*mut c_char>;

/// Renders the node's current metrics as an `OpenMetrics` text document.
///
/// Metrics are process-wide, so this takes no node handle.
/// It requires `OpenMetrics` to be enabled in the tracing config.
///
/// # Returns
///
/// A [`FfiGetOpenMetricsResult`] containing a pointer to an allocated C string
/// on success, or an [`OperationStatus`] error on failure. If `OpenMetrics` is
/// not enabled, the error code is `NotFound`.
///
/// # Memory Management
///
/// This function allocates memory for the output C string.
/// The caller must free this memory using the
/// [`free_cstring`](super::free_cstring) function.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub extern "C" fn get_open_metrics() -> FfiGetOpenMetricsResult {
    let open_metrics = match read_open_metrics() {
        Ok(Some(open_metrics)) => open_metrics,
        Ok(None) => {
            return FfiGetOpenMetricsResult::err(OperationStatus::error(
                OperationStatusCode::NotFound,
                "OpenMetrics is not enabled",
            ));
        }
        Err(error) => {
            return FfiGetOpenMetricsResult::err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                format!("Failed to render OpenMetrics: {error}"),
            ));
        }
    };

    match CString::new(open_metrics) {
        Ok(open_metrics) => FfiGetOpenMetricsResult::ok(open_metrics.into_raw()),
        Err(error) => FfiGetOpenMetricsResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )),
    }
}
