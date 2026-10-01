//! Conversion of the recovery state of a wallet of v0.3.0
//!
//! A wallet of v0.3.0 identifies its keys by the ids of the preload KMS
//! backend, and has no HD keys.

use bytes::Bytes;
use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
use lb_core::header::HeaderId;
use lb_key_management_system_service::backend::preload;
use lb_wallet::{Vouchers, WalletState};
use overwatch::DynError;
use serde::Deserialize;

use crate::{
    KeyId, hd,
    states::{PendingClaims, RecoveryState, VoucherIndex},
};

/// The recovery state of a wallet of v0.3.0
#[derive(Deserialize)]
struct RecoveryStateV0_3_0 {
    next_new_voucher_index: VoucherIndex,
    vouchers: Vouchers<(preload::KeyId, VoucherIndex)>,
    lib_wallet_state: Option<(HeaderId, WalletState)>,
    pending_claims: PendingClaims,
}

/// Converts the encoded recovery state of a wallet of v0.3.0 into the current
/// encoding.
///
/// The keys of the vouchers become static keys. The wallet starts with the
/// next HD indices of a new wallet, [`hd::INITIAL_NEXT_RECEIVE_INDEX`] and
/// [`hd::INITIAL_NEXT_CHANGE_INDEX`].
///
/// # Errors
///
/// Returns an error if `bytes` is not a recovery state of v0.3.0.
pub fn migrate_recovery_state(bytes: &[u8]) -> Result<Bytes, DynError> {
    let RecoveryStateV0_3_0 {
        next_new_voucher_index,
        vouchers,
        lib_wallet_state,
        pending_claims,
    } = RecoveryStateV0_3_0::from_bytes(bytes)?;

    let state = RecoveryState::new(
        next_new_voucher_index,
        // Convert the hex-encoded `KeyId` strings into [`KeyId::Static`] values.
        Vouchers::new(
            vouchers
                .into_iter()
                .map(|(cm, nf, (key_id, index))| (cm, nf, (KeyId::Static(key_id), index))),
        ),
        lib_wallet_state,
        pending_claims,
        // Add the next receive/change indices with their initial values.
        hd::INITIAL_NEXT_RECEIVE_INDEX,
        hd::INITIAL_NEXT_CHANGE_INDEX,
    );
    Ok(state.to_bytes()?)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lb_core::mantle::ops::leader_claim::{VoucherCm, VoucherNullifier};
    use lb_groth16::Fr;
    use serde::Serialize;

    use super::*;

    /// A [`Vouchers`] whose fields can be set and read, encoded the same way
    #[derive(Serialize, Deserialize)]
    struct TestVouchers<Id> {
        vouchers: HashMap<VoucherCm, Id>,
        voucher_nullifiers: HashMap<VoucherNullifier, VoucherCm>,
    }

    /// A [`RecoveryStateV0_3_0`] that can be encoded
    #[derive(Serialize)]
    struct TestRecoveryStateV0_3_0 {
        next_new_voucher_index: VoucherIndex,
        vouchers: TestVouchers<(preload::KeyId, VoucherIndex)>,
        lib_wallet_state: Option<(HeaderId, WalletState)>,
        pending_claims: PendingClaims,
    }

    #[test]
    fn recovery_state_migration_from_v0_3_0() {
        let cm = VoucherCm::from(Fr::from(1u8));
        let nf = VoucherNullifier::from(Fr::from(2u8));
        let old = TestRecoveryStateV0_3_0 {
            next_new_voucher_index: 7,
            vouchers: TestVouchers {
                vouchers: [(cm, ("aa70".to_owned(), 6))].into(),
                voucher_nullifiers: [(nf, cm)].into(),
            },
            lib_wallet_state: None,
            pending_claims: PendingClaims::default(),
        };

        let bytes = migrate_recovery_state(&old.to_bytes().unwrap()).unwrap();
        let state = RecoveryState::from_bytes(&bytes).unwrap();

        assert_eq!(state.next_receive_index(), hd::INITIAL_NEXT_RECEIVE_INDEX);
        assert_eq!(state.next_change_index(), hd::INITIAL_NEXT_CHANGE_INDEX);
        let vouchers = TestVouchers::<(KeyId, VoucherIndex)>::from_bytes(
            &state.vouchers().to_bytes().unwrap(),
        )
        .unwrap();
        assert_eq!(vouchers.vouchers, [(cm, (KeyId::from("aa70"), 6))].into());
        assert_eq!(vouchers.voucher_nullifiers, [(nf, cm)].into());
    }

    #[test]
    fn current_state_is_rejected() {
        use overwatch::services::state::ServiceState as _;

        let settings = crate::WalletServiceSettings {
            static_keys: HashMap::new(),
            unspendable_keys: std::collections::HashSet::new(),
            recovery_data: lb_services_utils::overwatch::RecoveryData::default(),
            pending_note_expiry_blocks: 10,
        };
        let state = RecoveryState::from_settings(&settings).unwrap();

        migrate_recovery_state(&state.to_bytes().unwrap()).unwrap_err();
    }
}
