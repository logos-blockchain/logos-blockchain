use std::ffi::{CString, c_char};

/// Frees memory allocated for a given pointer.
///
/// # Arguments
///
/// * `pointer` - A pointer to the memory to be freed.
pub fn free<Type>(pointer: *mut Type) {
    if !pointer.is_null() {
        unsafe {
            drop(Box::from_raw(pointer));
        }
    }
}

/// # Safety
/// It's up to the caller to pass a proper pointer, if somehow from c/c++ side
/// this is called with a type which doesn't come from a returned `CString` it
/// will cause a segfault.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_cstring(block: *mut c_char) {
    if block.is_null() {
        log::error!("Trying to free a null 'Block' pointer. Exiting");
        return;
    }
    drop(unsafe { CString::from_raw(block) });
}
