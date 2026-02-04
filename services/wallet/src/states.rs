use lb_core::{
    header::HeaderId,
    mantle::ops::leader_claim::{VoucherCm, VoucherNullifier},
};
use lb_ledger::LedgerState;
use lb_wallet::{Vouchers, WalletBlock, WalletError};
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
}

impl overwatch::services::state::ServiceState for RecoveryState {
    type Settings = WalletServiceSettings;
    type Error = WalletServiceError;

    fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            next_new_voucher_index: 0,
            vouchers: Vouchers::default(),
        })
    }
}

/// Provides operations on the states that must be synced to [`RecoveryState`].
pub struct ServiceState<'u> {
    next_new_voucher_index: VoucherIndex,
    wallet: Wallet,
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
        Self {
            next_new_voucher_index: state.next_new_voucher_index,
            wallet: Wallet::from_lib(
                settings
                    .known_keys
                    .clone()
                    .into_iter()
                    .map(|(key_id, pk)| (pk, key_id)),
                state.vouchers,
                lib,
                lib_ledger,
            ),
            updater,
        }
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

    pub fn apply_block(
        &mut self,
        block: &WalletBlock,
        ledger: &LedgerState,
    ) -> Result<(), WalletError> {
        self.wallet.apply_block(block, ledger)?;
        self.update_state();
        Ok(())
    }

    pub fn prune_states(&mut self, pruned_blocks: impl IntoIterator<Item = HeaderId>) {
        self.wallet.prune_states(pruned_blocks);
        self.update_state();
    }

    pub const fn wallet(&self) -> &Wallet {
        &self.wallet
    }

    fn update_state(&self) {
        self.updater.update(Some(RecoveryState {
            next_new_voucher_index: self.next_new_voucher_index,
            vouchers: self.wallet.vouchers().clone(),
        }));
    }
}
