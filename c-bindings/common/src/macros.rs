/// Checks if a pointer is null and returns from the calling function with a
/// null-pointer error status.
///
/// Works with any return type that implements [`FfiReturn`](crate::FfiReturn).
///
/// # Arguments
///
/// - `$pointer`: The pointer expression to check.
/// - `$status`: The error type, which must implement
///   [`FfiError`](crate::FfiError).
#[macro_export]
macro_rules! return_error_if_null_pointer {
    ($pointer:expr, $status:ty) => {
        if $pointer.is_null() {
            let error = <$status as $crate::FfiError>::null_pointer(stringify!($pointer));
            // SAFETY: The error was just allocated by this library.
            return unsafe { $crate::FfiReturn::from_error(error) };
        }
    };
}

/// Unwraps a [`Result`], returning the [`Ok`] value, or converts the error
/// into the function's return type and returning early.
///
/// Works with any return type that implements [`FfiReturn`](crate::FfiReturn).
///
/// # Arguments
///
/// - `$result`: The `Result<T, Error>` expression to unwrap.
///
/// # Safety
/// Errors must own valid, unreleased allocations from this library. When the
/// calling function returns `()`, the error is released here.
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
                return $crate::FfiReturn::from_error(error);
            }
        }
    };
}
