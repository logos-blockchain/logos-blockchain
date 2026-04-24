use std::{collections::HashMap, num::NonZero};

use lb_core::mantle::Utxo;
use lb_testing_framework::configs::wallet::WalletAccount;

use crate::cucumber::error::StepError;

pub const DEFAULT_STORAGE_GAS_PRICE: u64 = 0;
pub const SCENARIO_FEE_ACCOUNT_NAME: &str = "__SCENARIO_FEE_ACCOUNT__";

const SCENARIO_FEE_ACCOUNT_INDEX: u64 = 1 << 63;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SponsoredGenesisFeeAccount {
    pub token_count: NonZero<usize>,
    pub token_value: NonZero<u64>,
}

#[derive(Debug, Default)]
pub struct ScenarioFeeState {
    pub sponsored_genesis_account: Option<SponsoredGenesisFeeAccount>,
    pub wallet_account: Option<WalletAccount>,
    pub encumbered_tokens_per_wallet: HashMap<String, Vec<Utxo>>,
}

impl ScenarioFeeState {
    pub const fn set_sponsored_genesis_account(
        &mut self,
        token_count: NonZero<usize>,
        token_value: NonZero<u64>,
    ) {
        self.sponsored_genesis_account = Some(SponsoredGenesisFeeAccount {
            token_count,
            token_value,
        });
    }
}

pub fn create_scenario_fee_wallet_account(
    token_value: NonZero<u64>,
) -> Result<WalletAccount, StepError> {
    WalletAccount::deterministic(SCENARIO_FEE_ACCOUNT_INDEX, token_value.get(), true).map_err(
        |source| StepError::LogicalError {
            message: format!("failed to derive scenario fee reserve account: {source}"),
        },
    )
}
