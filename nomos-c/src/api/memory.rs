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
