pub extern "C" fn free<Type>(ptr: *mut Type) {
    if !ptr.is_null() {
        unsafe {
            drop(Box::from_raw(ptr));
        }
    }
}
