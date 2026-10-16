//! Shared support for C bindings, without node or SQL dependencies.
//!
//! Each library defines its own status codes and exports its own cleanup
//! functions. This crate exports no C symbols. Returned allocations must be
//! released through the library that created them.

mod macros;
pub mod memory;
mod result;

use std::any::Any;

pub use result::FfiResult;

/// The error operations needed by the shared result and guard helpers.
///
/// # Safety
/// `null_pointer` and `from_panic` must return owned errors that can be safely
/// passed to `release` once.
pub unsafe trait FfiError: Sized {
    fn is_ok(&self) -> bool;
    fn null_pointer(name: &str) -> Self;
    fn from_panic(message: impl Into<String>) -> Self;

    /// Releases an error when the exported function has no return value.
    ///
    /// # Safety
    /// The error must own valid, unreleased allocations from this library.
    unsafe fn release(self);
}

/// Implemented by FFI return types that can be constructed from an
/// `Error`, enabling the `return_error_if_null_pointer!` and
/// `unwrap_or_return_error!` macros to work across all return types.
pub trait FfiReturn<Error> {
    /// # Safety
    /// The error must own valid, unreleased allocations from this library.
    /// A void return releases them rather than returning them to the caller.
    unsafe fn from_error(error: Error) -> Self;
}

impl<Value: Default, Error: Default> FfiReturn<Error> for FfiResult<Value, Error> {
    unsafe fn from_error(error: Error) -> Self {
        Self::err(error)
    }
}

impl<Error: FfiError> FfiReturn<Error> for () {
    /// There is nowhere to report the error, so it is released: dropping it
    /// would leak its message.
    unsafe fn from_error(error: Error) {
        // SAFETY: The caller transfers ownership of a valid error.
        unsafe { error.release() };
    }
}

/// Formats a caught panic for a library's own error status.
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");

    format!("Internal panic: {message}")
}
