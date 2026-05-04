use lb_core::{
    header::HeaderId,
    mantle::ops::leader_claim::{VoucherCm, VoucherNullifier},
};
use lb_ledger::LedgerState;
use lb_wallet::{Vouchers, WalletBlock, WalletError, WalletState};
use overwatch::services::state::StateUpdater;
use serde::{Deserialize, Serialize};

use crate::{KeyId, WalletServiceError, WalletServiceSettings};

type VoucherIndex = u64;
type VoucherId = (KeyId, VoucherIndex);
pub type Wallet = lb_wallet::Wallet<KeyId, VoucherId>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryState {
    next_new_voucher_index: VoucherIndex,
    vouchers: Vouchers<VoucherId>,
    /// [`WalletState`] at the last known LIB.
    /// `None` on fresh start; populated after the first LIB update.
    lib_wallet_state: Option<(HeaderId, WalletState)>,
}

impl overwatch::services::state::ServiceState for RecoveryState {
    type Settings = WalletServiceSettings;
    type Error = WalletServiceError;

    fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            next_new_voucher_index: 0,
            vouchers: Vouchers::default(),
            lib_wallet_state: None,
        })
    }
}

/// Provides operations on the states that must be synced to [`RecoveryState`].
pub struct ServiceState<'u> {
    next_new_voucher_index: VoucherIndex,
    wallet: Wallet,
    lib: HeaderId,
    updater: &'u StateUpdater<Option<RecoveryState>>,
}

impl<'u> ServiceState<'u> {
    pub fn new(
        state: RecoveryState,
        settings: &WalletServiceSettings,
        lib: HeaderId,
        lib_ledger: &LedgerState,
        updater: &'u StateUpdater<Option<RecoveryState>>,
    ) -> Self {
        let known_keys = settings
            .known_keys
            .iter()
            .map(|(key_id, pk)| (*pk, key_id.clone()));

        // Initialize [`Wallet`] either from the persisted [`WalletState`]
        // or from the current chain's LIB ledger state.
        let (wallet, wallet_lib) = match state.lib_wallet_state {
            Some((persisted_lib, wallet_state)) => (
                Wallet::from_lib_wallet_state(
                    known_keys,
                    state.vouchers,
                    persisted_lib,
                    wallet_state,
                ),
                persisted_lib,
            ),
            None => (
                Wallet::from_lib_ledger_state(known_keys, state.vouchers, lib, lib_ledger),
                lib,
            ),
        };

        Self {
            next_new_voucher_index: state.next_new_voucher_index,
            wallet,
            lib: wallet_lib,
            updater,
        }
    }

    pub const fn lib(&self) -> HeaderId {
        self.lib
    }

    pub fn get_and_inc_next_new_voucher_index(&mut self) -> VoucherIndex {
        let index = self.next_new_voucher_index;
        self.next_new_voucher_index += 1;
        self.update_state();
        index
    }

    pub fn add_known_voucher(&mut self, cm: VoucherCm, nf: VoucherNullifier, id: VoucherId) {
        self.wallet.add_known_voucher(cm, nf, id);
        self.update_state();
    }

    pub fn apply_block(&mut self, block: &WalletBlock) -> Result<(), WalletError> {
        self.wallet.apply_block(block)?;
        self.update_state();
        Ok(())
    }

    pub fn advance_lib(
        &mut self,
        new_lib: HeaderId,
        pruned_blocks: impl IntoIterator<Item = HeaderId>,
        pruned_nullifiers: impl IntoIterator<Item = VoucherNullifier>,
    ) {
        self.lib = new_lib;
        self.wallet.prune_states(pruned_blocks);
        self.wallet.prune_vouchers(pruned_nullifiers);
        self.update_state();
    }

    pub const fn wallet(&self) -> &Wallet {
        &self.wallet
    }

    fn update_state(&self) {
        let lib_wallet_state = self
            .wallet()
            .wallet_state_at(self.lib)
            .expect("WalletState at LIB must exist");

        self.updater.update(Some(RecoveryState {
            next_new_voucher_index: self.next_new_voucher_index,
            vouchers: self.wallet.vouchers().clone(),
            lib_wallet_state: Some((self.lib, lib_wallet_state)),
        }));
    }
}
