use std::collections::{HashMap, HashSet};

use lb_core::mantle::Utxo;
use lb_key_management_system_service::keys::ZkPublicKey;
use thiserror::Error;

use crate::common::wallet::WalletId;

pub type WalletUtxos = HashMap<WalletId, Vec<Utxo>>;

/// Wallet public keys whose chain UTXOs should be tracked as one test wallet.
#[derive(Debug, Clone)]
pub struct TrackedWalletKeys {
    wallet_id: WalletId,
    wallet_pks: HashSet<ZkPublicKey>,
}

impl TrackedWalletKeys {
    #[must_use]
    pub fn new(
        wallet_id: impl Into<WalletId>,
        wallet_pks: impl IntoIterator<Item = ZkPublicKey>,
    ) -> Self {
        Self {
            wallet_id: wallet_id.into(),
            wallet_pks: wallet_pks.into_iter().collect(),
        }
    }

    #[must_use]
    pub const fn wallet_id(&self) -> &WalletId {
        &self.wallet_id
    }

    pub(crate) fn wallet_pks(&self) -> impl Iterator<Item = ZkPublicKey> + '_ {
        self.wallet_pks.iter().copied()
    }
}

#[derive(Debug, Clone, Eq, Error, PartialEq)]
pub enum TrackedWalletKeysError {
    #[error(
        "Multiple wallets have the same public key {public_key:?}: '{first_wallet}' and \
        '{second_wallet}'"
    )]
    DuplicatePublicKey {
        public_key: ZkPublicKey,
        first_wallet: WalletId,
        second_wallet: WalletId,
    },
}
