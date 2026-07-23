use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{
    errors::{OperationStatus, OperationStatusCode},
    result::FfiResult,
};

/// Checks if a pointer is null and returns from the calling function with a
/// null-pointer error status.
///
/// Works with any return type that implements [`FfiReturn`], including
/// [`FfiResult`], [`OperationStatus`], and `()`.
///
/// # Arguments
///
/// - `$pointer`: The pointer expression to check.
#[macro_export]
macro_rules! return_error_if_null_pointer {
    ($pointer:expr) => {
        if $pointer.is_null() {
            return <_ as $crate::macros::FfiReturn>::from_operation_status(
                $crate::errors::OperationStatus::error(
                    $crate::errors::OperationStatusCode::NullPointer,
                    format!("Received a null `{}` pointer.", stringify!($pointer)),
                ),
            );
        }
    };
}

/// Unwraps a [`Result`], returning the [`Ok`] value, or converts the error
/// into the function's return type and returning early.
///
/// Works with any return type that implements [`FfiReturn`], including
/// [`FfiResult`], [`OperationStatus`], and `()`.
///
/// # Arguments
///
/// - `$result`: The `Result<T, OperationStatus>` expression to unwrap.
#[macro_export]
macro_rules! unwrap_or_return_error {
    ($result:expr) => {
        $crate::unwrap_or_return_error!($result, |_| {})
    };
    ($result:expr, $on_err:expr) => {
        match $result {
            Ok(value) => value,
            Err(error) => {
                $on_err(&error);
                return <_ as $crate::macros::FfiReturn>::from_operation_status(error);
            }
        }
    };
}

/// Implemented by FFI return types that can be constructed from an
/// [`OperationStatus`] error, enabling the `return_error_if_null_pointer!` and
/// `unwrap_or_return_error!` macros to work across all return types.
pub trait FfiReturn {
    fn from_operation_status(status: OperationStatus) -> Self;
}

impl<Type: Default> FfiReturn for FfiResult<Type, OperationStatus> {
    fn from_operation_status(status: OperationStatus) -> Self {
        Self::err(status)
    }
}

impl FfiReturn for OperationStatus {
    fn from_operation_status(status: OperationStatus) -> Self {
        status
    }
}

impl FfiReturn for () {
    fn from_operation_status(_status: OperationStatus) -> Self {}
}

/// Runs an FFI entry-point body, converting any panic into an [`OperationStatus`]
/// error instead of letting it unwind across the `extern "C"` boundary.
///
/// A panic that unwinds out of an `extern "C"` function aborts the process, so
/// every entry point that can panic — in practice anything that drives the async
/// runtime via `block_on` — should wrap its body in `guard_ffi`. A caught panic
/// is reported as [`OperationStatusCode::RuntimeError`]; the standard panic hook
/// still runs first, so the panic is logged with its backtrace as usual.
///
/// Note: this only makes panics recoverable once the process is *not* running the
/// `log_and_exit_hook` panic hook (which calls `process::exit`). The embedded
/// cdylib intentionally leaves that hook uninstalled — see
/// `logos_blockchain_node::install_panic_hook`.
pub fn guard_ffi<R: FfiReturn>(body: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| {
        R::from_operation_status(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            "A panic occurred while executing a node operation.",
        ))
    })
}
