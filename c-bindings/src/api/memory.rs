use std::ffi::{CString, c_char};

use crate::{OperationStatus, return_error_if_null_pointer};

/// Frees memory allocated for a given pointer.
///
/// # Arguments
///
/// - `pointer`: A pointer to the memory to be freed.
pub fn free<Type>(pointer: *mut Type) -> OperationStatus {
    if pointer.is_null() {
        return OperationStatus::NullPointer;
    }
    unsafe { drop(Box::from_raw(pointer)) };
    OperationStatus::Ok
}

/// Frees a C string allocated by this library.
///
/// # Arguments
///
/// - `pointer`: A pointer to a C string previously allocated by this library.
///
/// # Returns
///
/// An [`OperationStatus`] indicating success or failure.
///
/// # Safety
///
/// The pointer must originate from a [`CString`] allocated by this library.
/// Passing a pointer from any other source will cause undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_cstring(pointer: *mut c_char) -> OperationStatus {
    return_error_if_null_pointer!("free_cstring", pointer);
    drop(unsafe { CString::from_raw(pointer) });
    OperationStatus::Ok
}
