use std::ffi::{CString, c_char};

use crate::{OperationStatus, return_error_if_null_pointer};

/// Frees memory allocated for a given pointer.
///
/// # Arguments
///
/// * `pointer` - A pointer to the memory to be freed.
pub fn free<Type>(pointer: *mut Type) -> OperationStatus {
    if pointer.is_null() {
        return OperationStatus::NullPointer;
    }
    unsafe { drop(Box::from_raw(pointer)) };
    OperationStatus::Ok
}

/// # Safety
/// It's up to the caller to pass a proper pointer, if somehow from c/c++ side
/// this is called with a type which doesn't come from a returned `CString` it
/// will cause a segfault.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_cstring(block: *mut c_char) -> OperationStatus {
    return_error_if_null_pointer!("free_cstring", block);
    drop(unsafe { CString::from_raw(block) });
    OperationStatus::Ok
}
