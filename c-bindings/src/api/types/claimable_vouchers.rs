use std::ptr;

use crate::api::cryptarchia::{Hash, HeaderId};

#[repr(C)]
pub struct ClaimableVoucher {
    pub commitment: Hash,
    pub nullifier: Hash,
}

#[repr(C)]
pub struct ClaimableVouchers {
    pub tip: HeaderId,
    pub vouchers: *mut ClaimableVoucher,
    pub len: usize,
}

impl Default for ClaimableVouchers {
    fn default() -> Self {
        Self {
            tip: [0; 32],
            vouchers: ptr::null_mut(),
            len: 0,
        }
    }
}
