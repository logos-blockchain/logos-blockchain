use std::ptr;

#[repr(C)]
pub struct KnownAddresses {
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
