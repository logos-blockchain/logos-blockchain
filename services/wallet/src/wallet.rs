use std::ops::Deref;

use lb_core::{header::HeaderId, mantle::ops::leader_claim::VoucherSecret};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_ledger::LedgerState;
use lb_wallet::{WalletBlock, WalletError};
use overwatch::services::state::StateUpdater;

use crate::state::State;

/// Wallet wrapper to make sure that the service state is updated
/// whenever the wallet is mutated.
pub struct Wallet {
    wallet: lb_wallet::Wallet,
    state_updater: StateUpdater<Option<State>>,
}

/// Implements [`Deref`] to provide transparent read-only access
/// to [`lb_wallet::Wallet`].
/// All mutations must go through the overriding methods below.
impl Deref for Wallet {
    type Target = lb_wallet::Wallet;

    fn deref(&self) -> &Self::Target {
        &self.wallet
    }
}

impl Wallet {
    #[must_use]
    pub fn from_lib(
        known_keys: impl IntoIterator<Item = ZkPublicKey>,
        known_voucher_secrets: impl IntoIterator<Item = VoucherSecret>,
        lib: HeaderId,
        ledger: &LedgerState,
        state_updater: StateUpdater<Option<State>>,
    ) -> Self {
        Self {
            wallet: lb_wallet::Wallet::from_lib(known_keys, known_voucher_secrets, lib, ledger),
            state_updater,
        }
    }

    pub fn add_known_voucher_secret(&mut self, secret: VoucherSecret) {
        self.wallet.add_known_voucher_secret(secret);
        self.state_updater
            .update(Some(State::from_wallet(&self.wallet)));
    }

    pub fn apply_block(
        &mut self,
        block: &WalletBlock,
        ledger: &LedgerState,
    ) -> Result<(), WalletError> {
        self.wallet.apply_block(block, ledger)?;
        self.state_updater
            .update(Some(State::from_wallet(&self.wallet)));
        Ok(())
    }

    pub fn prune_states(&mut self, pruned_blocks: impl IntoIterator<Item = HeaderId>) {
        self.wallet.prune_states(pruned_blocks);
        self.state_updater
            .update(Some(State::from_wallet(&self.wallet)));
    }
}
