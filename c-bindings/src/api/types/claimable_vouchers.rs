use std::ptr;

use crate::api::{
    cryptarchia::{Hash, HeaderId},
    types::value::Value,
};

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
    /// What a single voucher pays out at `tip`.
    ///
    /// The reward pool is split evenly across every unclaimed voucher on the
    /// chain, so this is the same for each of `vouchers` and it moves as other
    /// leaders claim. It is a snapshot at `tip`, not a guarantee of what a
    /// claim submitted now will settle for.
    pub reward_amount: Value,
    /// `reward_amount` times `len`: what this wallet could claim in total at
    /// `tip`.
    pub total_claimable: Value,
}

impl Default for ClaimableVouchers {
    fn default() -> Self {
        Self {
            tip: [0; 32],
            vouchers: ptr::null_mut(),
            len: 0,
            reward_amount: 0,
            total_claimable: 0,
        }
    }
}
