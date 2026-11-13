use std::ffi::{CString, c_char};

/// Allocates a C string, escaping embedded NUL bytes as `\0`.
///
/// The returned pointer is never null. The caller owns it and must release
/// it with [`free_cstring`] from the same library.
pub fn escaped_cstring(value: impl Into<String>) -> *mut c_char {
    CString::new(value.into().replace('\0', "\\0"))
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
pub unsafe fn free<Type>(pointer: *mut Type) {
    // SAFETY: The caller returns ownership of the allocation to Rust.
    drop(unsafe { Box::from_raw(pointer) });
}
