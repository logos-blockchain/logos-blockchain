/// Logs a message to stderr with level, scope, and message.
///
/// # Arguments
///
/// - `$level`: An identifier for the log level (`ERROR`, `WARN`, etc.). Printed
///   uppercase, left-padded to 5 characters.
/// - `$scope`: A string literal identifying the call site (typically the
///   function name).
/// - `$($arg:tt)*`: Format string and arguments, same syntax as [`eprintln!`].
///
/// # Example
///
/// ```rust,ignore
/// logging::log!(ERROR, "start_lb_node", "Failed to parse config: {e}");
/// // stderr: ERROR [start_lb_node] Failed to parse config: ...
/// logging::log!(WARN, "subscribe_to_new_blocks_sync", "Block stream closed");
/// // stderr: WARN  [subscribe_to_new_blocks_sync] Block stream closed
/// ```
macro_rules! log {
    ($level:ident, $scope:literal, $($arg:tt)*) => {
        ::std::eprintln!("{:<5} [{}] {}", stringify!($level), $scope, ::std::format_args!($($arg)*));
    };
}

macro_rules! error {
    ($scope:literal, $($arg:tt)*) => {
        $crate::logging::log!(ERROR, $scope, $($arg)*)
    };
}

macro_rules! warning {
    ($scope:literal, $($arg:tt)*) => {
        $crate::logging::log!(WARN, $scope, $($arg)*)
    };
}

pub(crate) use error;
pub(crate) use log;
pub(crate) use warning;
