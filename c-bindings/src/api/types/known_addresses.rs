use std::ptr;

#[repr(C)]
pub struct KnownAddresses {
    /// When the list is empty this pointer is not null, but it points to
    /// nothing: check the length before reading through it.
    pub addresses: *mut *mut u8,
    pub len: usize,
}

impl Default for KnownAddresses {
    fn default() -> Self {
        Self {
            addresses: ptr::null_mut(),
            len: 0,
        }
    }
}
