use std::ffi::{CString, c_char};

use crate::{OperationStatus, return_error_if_null_pointer};

/// Frees memory allocated for a given pointer.
///
/// A null pointer frees nothing and is reported as a `NullPointer` error.
///
/// # Arguments
///
/// - `pointer`: A pointer to the memory to be freed, or null.
pub fn free<Type>(pointer: *mut Type) -> OperationStatus {
    return_error_if_null_pointer!(pointer);
    unsafe { drop(Box::from_raw(pointer)) };
    OperationStatus::OK
}

/// Frees a C string allocated by this library.
///
/// # Arguments
///
/// - `pointer`: A pointer to a C string previously allocated by this library.
///
/// # Returns
///
/// An [`OperationStatus`] indicating success or failure. A null `pointer`
/// frees nothing and returns a `NullPointer` error. Like any error, it owns a
/// message: release it with [`free_operation_status`].
///
/// # Safety
///
/// A non-null pointer must originate from a [`CString`] allocated by this
/// library and must not have been freed already.
/// Passing a pointer from any other source will cause undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_cstring(pointer: *mut c_char) -> OperationStatus {
    return_error_if_null_pointer!(pointer);
    drop(unsafe { CString::from_raw(pointer) });
    OperationStatus::OK
}

/// Releases an [`OperationStatus`] returned by this library.
///
/// The only thing a status owns is its `message`, which is null on success
/// and on a few errors. This frees it when there is one, so any status —
/// success or error — can be released without looking inside it.
///
/// # Arguments
///
/// - `status`: A status returned by any function of this library, including
///   the `error` field of a result.
///
/// # Safety
///
/// `status` must come from this library and must not have been released
/// already, either through this function or by passing its `message` to
/// [`free_cstring`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_operation_status(status: OperationStatus) {
    if !status.message.is_null() {
        drop(unsafe { CString::from_raw(status.message) });
    }
}
