use std::collections::HashSet;

use lb_core::mantle::ops::leader_claim::VoucherSecret;
use lb_wallet::Wallet;
use overwatch::services::state::ServiceState;
use serde::{Deserialize, Serialize};

use crate::WalletServiceSettings;

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct State {
    voucher_secrets: HashSet<VoucherSecret>,
}

#[derive(thiserror::Error, Debug)]
pub enum StateError {}

impl ServiceState for State {
    type Settings = WalletServiceSettings;
    type Error = StateError;

    fn from_settings(_: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self::default())
    }
}

impl State {
    pub fn from_wallet(wallet: &Wallet) -> Self {
        Self {
            voucher_secrets: wallet.known_voucher_secrets().clone(),
        }
    }

    pub fn voucher_secrets(&self) -> impl Iterator<Item = &VoucherSecret> {
        self.voucher_secrets.iter()
    }
}
