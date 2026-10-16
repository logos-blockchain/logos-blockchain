use std::ffi::c_char;

use lb_c_common::{FfiError, FfiReturn, memory};
use logos_sql::Error;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub enum SqlStatusCode {
    #[default]
    SqlOk = 0,
    SqlNullPointer = 1,
    SqlInvalidArgument = 2,
    SqlFailure = 3,
    SqlNotReady = 4,
    SqlRuntimeStopped = 5,
    SqlTimeout = 6,
    SqlPanic = 7,
}

/// Check `code` before accessing a result's value. Release the message with
/// `logos_sql_free_status`, including on failure.
#[derive(Default)]
#[repr(C)]
pub struct SqlStatus {
    pub code: SqlStatusCode,
    pub message: *mut c_char,
}

impl SqlStatus {
    pub(crate) fn error(code: SqlStatusCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: memory::error_message(message),
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::error(SqlStatusCode::SqlInvalidArgument, message)
    }

    pub(crate) fn failure(message: impl Into<String>) -> Self {
        Self::error(SqlStatusCode::SqlFailure, message)
    }
}

// SAFETY: Both constructors return a newly allocated message owned by the
// status.
unsafe impl FfiError for SqlStatus {
    fn is_ok(&self) -> bool {
        self.code == SqlStatusCode::SqlOk
    }

    fn null_pointer(name: &str) -> Self {
        Self::error(SqlStatusCode::SqlNullPointer, format!("{name} is null"))
    }

    fn from_panic(message: impl Into<String>) -> Self {
        Self::error(SqlStatusCode::SqlPanic, message)
    }

    unsafe fn release(self) {
        // SAFETY: The caller transfers ownership of this status's message.
        unsafe { memory::free_cstring(self.message) };
    }
}

impl FfiReturn<Self> for SqlStatus {
    unsafe fn from_error(error: Self) -> Self {
        error
    }
}

impl From<Error> for SqlStatus {
    fn from(error: Error) -> Self {
        let code = match &error {
            Error::SequencerNotReady => SqlStatusCode::SqlNotReady,
            Error::RuntimeHalted | Error::RuntimeStopped => SqlStatusCode::SqlRuntimeStopped,
            _ => SqlStatusCode::SqlFailure,
        };

        Self::error(code, error.to_string())
    }
}
