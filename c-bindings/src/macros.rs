/// Checks if a pointer is null, logs an error, and returns from the calling
/// function.
///
/// # Arguments
///
/// - `$context`: A string literal describing where the error occurred, used in
///   the log message.
/// - `$pointer`: The pointer expression to check.
#[macro_export]
macro_rules! return_error_if_null_pointer {
    ($context:literal, $pointer:expr) => {
        if $pointer.is_null() {
            log::error!(
                "[{}] Received a null `{}` pointer. Exiting.",
                $context,
                stringify!($pointer)
            );
            return $crate::result::ValueResult::from_error(
                $crate::errors::OperationStatus::NullPointer,
            );
        }
    };
}

/// Unwraps a [`Result`], returning the [`Ok`] value, or converts the error into
/// a [`ValueResult`] and returning early from the calling function.
///
/// # Arguments
///
/// - `$result`: The `Result` expression to unwrap.
#[macro_export]
macro_rules! unwrap_or_return_err {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(error) => return $crate::result::ValueResult::from_error(error),
        }
    };
}
