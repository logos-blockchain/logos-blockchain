use std::collections::HashMap;

use lb_core::mantle::ops::leader_claim::{VoucherCm, VoucherNullifier};
use serde::{Deserialize, Serialize};

/// Holds voucher indices for
/// - generating new vouchers
/// - looking up existing voucher IDs by commitment or nullifier
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Vouchers<Id> {
    vouchers: HashMap<VoucherCm, Id>,
    voucher_nullifiers: HashMap<VoucherNullifier, VoucherCm>,
}

impl<Id> Vouchers<Id> {
    #[cfg(test)]
    pub fn new(vouchers: impl IntoIterator<Item = (VoucherCm, VoucherNullifier, Id)>) -> Self {
        let (vouchers, voucher_nullifiers) = vouchers.into_iter().fold(
            (HashMap::new(), HashMap::new()),
            |(mut vouchers, mut voucher_nullifiers), (cm, nf, id)| {
                vouchers.insert(cm, id);
                voucher_nullifiers.insert(nf, cm);
                (vouchers, voucher_nullifiers)
            },
        );
        Self {
            vouchers,
            voucher_nullifiers,
        }
    }
    pub(crate) fn insert(&mut self, cm: VoucherCm, nf: VoucherNullifier, id: Id) {
        self.vouchers.insert(cm, id);
        self.voucher_nullifiers.insert(nf, cm);
    }

    pub(crate) fn get(&self, cm: &VoucherCm) -> Option<&Id> {
        self.vouchers.get(cm)
    }

    pub(crate) fn get_by_nullifier(&self, nf: &VoucherNullifier) -> Option<&Id> {
        self.get(self.voucher_nullifiers.get(nf)?)
    }

    pub(crate) fn remove_by_nullifier(&mut self, nf: &VoucherNullifier) -> Option<Id> {
        let cm = self.voucher_nullifiers.remove(nf)?;
        self.vouchers.remove(&cm)
    }

    pub(crate) fn commitments_and_nullifiers(
        &self,
    ) -> impl Iterator<Item = (&VoucherNullifier, &VoucherCm)> {
        self.voucher_nullifiers.iter()
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.vouchers.len()
    }
}
