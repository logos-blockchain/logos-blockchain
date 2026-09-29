//! The upgrade of the recovery state of a wallet that has no HD keys
//!
//! Such a wallet derived all its vouchers from a single key, which it
//! identified by the hex of its public key. The upgraded state identifies that
//! key by the name that it is loaded under, and starts the indices of the HD
//! keys from the beginning. The vouchers are kept, for the wallet to claim
//! them with the key that they were derived from.

use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
use lb_core::header::HeaderId;
use lb_wallet::{Vouchers, WalletState};
use serde::Deserialize;

use super::{PendingClaims, RecoveryState, VoucherIndex};
use crate::KeyId;

/// The recovery state of a wallet that has no HD keys
#[derive(Deserialize)]
struct LegacyRecoveryState {
    /// Not kept, since the new vouchers are derived from another key.
    #[expect(dead_code, reason = "The field is part of the encoding.")]
    next_new_voucher_index: VoucherIndex,
    vouchers: Vouchers<(String, VoucherIndex)>,
    lib_wallet_state: Option<(HeaderId, WalletState)>,
    pending_claims: PendingClaims,
}

/// Upgrades the encoded recovery state of a wallet that has no HD keys.
///
/// `voucher_master_key_id` is the id of the key that the vouchers of the
/// state were derived from.
pub fn upgrade_recovery_state(
    legacy_state: &[u8],
    voucher_master_key_id: &KeyId,
) -> Result<bytes::Bytes, lb_binary_codec::bincode::Error> {
    let LegacyRecoveryState {
        vouchers,
        lib_wallet_state,
        pending_claims,
        ..
    } = LegacyRecoveryState::from_bytes(legacy_state)?;

    RecoveryState {
        next_receive_index: 0,
        next_change_index: 0,
        next_new_voucher_index: 0,
        vouchers: vouchers.map_ids(|(_, index)| (voucher_master_key_id.clone(), index)),
        lib_wallet_state,
        pending_claims,
    }
    .to_bytes()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lb_core::mantle::ops::leader_claim::{VoucherCm, VoucherNullifier};
    use lb_groth16::Fr;
    use serde::Serialize;

    use super::*;

    /// The recovery state as the wallet encoded it before the upgrade
    #[derive(Serialize)]
    struct EncodedLegacyRecoveryState {
        next_new_voucher_index: VoucherIndex,
        vouchers: EncodedVouchers<(String, VoucherIndex)>,
        lib_wallet_state: Option<(HeaderId, WalletState)>,
        pending_claims: PendingClaims,
    }

    /// The recovery state as the wallet encodes it
    #[derive(Deserialize)]
    struct EncodedRecoveryState {
        next_receive_index: u32,
        next_change_index: u32,
        next_new_voucher_index: VoucherIndex,
        vouchers: EncodedVouchers<(KeyId, VoucherIndex)>,
        lib_wallet_state: Option<(HeaderId, WalletState)>,
        pending_claims: PendingClaims,
    }

    /// The vouchers as [`Vouchers`] encodes them
    #[derive(Serialize, Deserialize)]
    struct EncodedVouchers<Id> {
        vouchers: HashMap<VoucherCm, Id>,
        voucher_nullifiers: HashMap<VoucherNullifier, VoucherCm>,
    }

    fn legacy_state(vouchers: &[(VoucherCm, VoucherNullifier, VoucherIndex)]) -> bytes::Bytes {
        let mut pending_claims = PendingClaims::default();
        for (_, nf, _) in vouchers {
            pending_claims.reserve(*nf);
        }
        EncodedLegacyRecoveryState {
            next_new_voucher_index: 8,
            vouchers: EncodedVouchers {
                vouchers: vouchers
                    .iter()
                    .map(|(cm, _, index)| (*cm, ("3fa9".to_owned(), *index)))
                    .collect(),
                voucher_nullifiers: vouchers.iter().map(|(cm, nf, _)| (*nf, *cm)).collect(),
            },
            lib_wallet_state: None,
            pending_claims,
        }
        .to_bytes()
        .unwrap()
    }

    fn upgrade(legacy_state: &[u8]) -> EncodedRecoveryState {
        let state = upgrade_recovery_state(legacy_state, &"VoucherMaster".into()).unwrap();
        EncodedRecoveryState::from_bytes(&state).unwrap()
    }

    #[test]
    fn vouchers_are_kept_under_the_name_of_their_key() {
        let cm = VoucherCm::from(Fr::from(1u64));
        let nf = VoucherNullifier::from(Fr::from(2u64));

        let state = upgrade(&legacy_state(&[(cm, nf, 7)]));

        assert_eq!(
            state.vouchers.vouchers,
            [(cm, (KeyId::Name("VoucherMaster".to_owned()), 7))].into()
        );
        assert_eq!(state.vouchers.voucher_nullifiers, [(nf, cm)].into());
        assert!(state.pending_claims.is_reserved(&nf));
        assert!(state.lib_wallet_state.is_none());
    }

    #[test]
    fn indices_start_from_the_beginning() {
        let state = upgrade(&legacy_state(&[]));

        assert_eq!(state.next_new_voucher_index, 0);
        assert_eq!(state.next_receive_index, 0);
        assert_eq!(state.next_change_index, 0);
    }

    #[test]
    fn upgraded_state_is_read_by_the_wallet() {
        let cm = VoucherCm::from(Fr::from(1u64));
        let nf = VoucherNullifier::from(Fr::from(2u64));
        let state =
            upgrade_recovery_state(&legacy_state(&[(cm, nf, 7)]), &"VoucherMaster".into()).unwrap();

        let state = RecoveryState::from_bytes(&state).unwrap();

        assert_eq!(state.vouchers.count(), 1);
    }
}
