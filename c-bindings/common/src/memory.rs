use std::ffi::{CString, c_char};

/// A C string cannot hold a NUL byte, but an error message is free to
/// carry one: many of them echo input the caller or a file supplied.
/// Escaping keeps the message readable and this function infallible.
pub fn error_message(message: impl Into<String>) -> *mut c_char {
    CString::new(message.into().replace('\0', "\\0"))
        .unwrap_or_default()
        .into_raw()
}

/// Frees a C string allocated by this library. Null is a no-op.
///
/// # Safety
/// A non-null pointer must come from `CString::into_raw` in the calling library
/// and must not have been released already.
pub unsafe fn free_cstring(pointer: *mut c_char) {
    if !pointer.is_null() {
        // SAFETY: The caller owns this allocation and transfers it back to Rust.
        drop(unsafe { CString::from_raw(pointer) });
    }
}

/// Frees memory allocated for a given pointer.
///
/// # Safety
/// The pointer must come from `Box::into_raw` for this type in the calling
/// library. It must be non-null, unused by other calls, and not already
/// released.
pub unsafe fn free<Value>(pointer: *mut Value) {
    // SAFETY: The caller returns ownership of the allocation to Rust.
    drop(unsafe { Box::from_raw(pointer) });
}
