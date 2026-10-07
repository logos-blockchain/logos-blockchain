use std::ffi::{CString, c_char};

use lb_c_macros::panic_to_error;

use crate::{OperationStatus, return_error_if_null_pointer};

/// Frees memory allocated for a given pointer.
///
/// A null pointer frees nothing and is reported as a `NullPointer` error.
///
/// # Arguments
///
/// - `pointer`: A pointer to the memory to be freed, or null.
///
/// # Safety
///
/// A non-null `pointer` must have been produced by `Box::into_raw` for a
/// `Type` and must not have been freed already.
pub unsafe fn free<Type>(pointer: *mut Type) -> OperationStatus {
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
/// message: release it with
/// [`free_operation_status`](crate::errors::free_operation_status).
///
/// # Safety
///
/// A non-null pointer must originate from a [`CString`] allocated by this
/// library and must not have been freed already.
/// Passing a pointer from any other source will cause undefined behavior.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_cstring(pointer: *mut c_char) -> OperationStatus {
    return_error_if_null_pointer!(pointer);
    drop(unsafe { CString::from_raw(pointer) });
    OperationStatus::OK
}
