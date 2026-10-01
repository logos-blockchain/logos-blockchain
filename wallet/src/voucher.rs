use std::collections::HashMap;

use lb_core::mantle::ops::leader_claim::{VoucherCm, VoucherNullifier};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug)]
pub struct Voucher {
    pub nullifier: VoucherNullifier,
    pub commitment: VoucherCm,
}

/// Holds voucher indices for
/// - generating new vouchers
/// - looking up existing voucher IDs by commitment or nullifier
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vouchers<Id> {
    vouchers: HashMap<VoucherCm, Id>,
    voucher_nullifiers: HashMap<VoucherNullifier, VoucherCm>,
}

impl<Id> Default for Vouchers<Id> {
    fn default() -> Self {
        Self {
            vouchers: HashMap::new(),
            voucher_nullifiers: HashMap::new(),
        }
    }
}

impl<Id> Vouchers<Id> {
    #[must_use]
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

    pub(crate) fn commitments_and_nullifiers(&self) -> impl Iterator<Item = Voucher> + '_ {
        self.voucher_nullifiers
            .iter()
            .map(|(&nullifier, &commitment)| Voucher {
                nullifier,
                commitment,
            })
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.vouchers.len()
    }
}

impl<Id> IntoIterator for Vouchers<Id> {
    type Item = (VoucherCm, VoucherNullifier, Id);
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        let Self {
            mut vouchers,
            voucher_nullifiers,
        } = self;
        voucher_nullifiers
            .into_iter()
            .filter_map(|(nf, cm)| Some((cm, nf, vouchers.remove(&cm)?)))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;

    use super::*;

    #[test]
    fn vouchers_are_iterated_as_they_are_given() {
        let entries = [1u8, 2].map(|seed| {
            (
                VoucherCm::from(Fr::from(seed)),
                VoucherNullifier::from(Fr::from(seed + 10)),
                u64::from(seed),
            )
        });

        let mut iterated = Vouchers::new(entries).into_iter().collect::<Vec<_>>();
        iterated.sort_by_key(|(_, _, id)| *id);

        assert_eq!(iterated, entries);
    }
}
