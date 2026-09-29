//! The HD keys of the wallet
//!
//! They are the note keys of a single account. Receive addresses are handed
//! out in the order of their index, and so are change addresses.
//!
//! The voucher secrets are derived from the voucher master key of the same
//! account.

use lb_key_management_system_service::hd::{HardenedIndex, NoteRole, Path, u31};

/// The index of a note key, which is the child number of the last level of
/// its path.
pub type Index = u32;

/// The account that the wallet operates on
const ACCOUNT: HardenedIndex = HardenedIndex::new(u31::new(0));

/// The path of the receive address at the index
#[must_use]
pub fn receive_path(index: Index) -> Path {
    note_path(NoteRole::Receive, index)
}

/// The path of the change address at the index
#[must_use]
pub fn change_path(index: Index) -> Path {
    note_path(NoteRole::Change, index)
}

/// The path of the voucher master key
#[must_use]
pub const fn voucher_master_path() -> Path {
    Path::VoucherMaster { account: ACCOUNT }
}

fn note_path(role: NoteRole, index: Index) -> Path {
    let index = u31::try_new(index).expect("Index of a note key is below 2^31");
    Path::Note {
        account: ACCOUNT,
        role,
        index: HardenedIndex::new(index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_in_account_0() {
        assert_eq!(receive_path(0).to_string(), "m/154'/0'/0'/0'");
        assert_eq!(receive_path(7).to_string(), "m/154'/0'/0'/7'");
        assert_eq!(change_path(0).to_string(), "m/154'/0'/1'/0'");
        assert_eq!(change_path(7).to_string(), "m/154'/0'/1'/7'");
        assert_eq!(voucher_master_path().to_string(), "m/154'/0'/2'");
    }
}
