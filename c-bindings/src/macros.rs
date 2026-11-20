/// Checks if a pointer is null and returns from the calling function with a
/// null-pointer error status.
///
/// Works with any return type that implements
/// [`FfiReturn`](lb_c_common::FfiReturn), including
/// [`FfiResult`](crate::FfiResult),
/// [`OperationStatus`](crate::OperationStatus), and `()`.
///
/// # Arguments
///
/// - `$pointer`: The pointer expression to check.
#[macro_export]
macro_rules! return_error_if_null_pointer {
    ($pointer:expr) => {
        ::lb_c_common::return_error_if_null_pointer!($pointer, $crate::OperationStatus);
    };
}

/// Unwraps a [`Result`], returning the [`Ok`] value, or converts the error
/// into the function's return type and returning early.
///
/// Works with any return type that implements
/// [`FfiReturn`](lb_c_common::FfiReturn), including
/// [`FfiResult`](crate::FfiResult),
/// [`OperationStatus`](crate::OperationStatus), and `()`.
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

                // SAFETY: Node operations return newly owned error statuses.
                return unsafe { ::lb_c_common::FfiReturn::from_error(error) };
            }
        }
    };
}
