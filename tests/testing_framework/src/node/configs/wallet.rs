use std::{collections::HashSet, num::NonZeroUsize};

use hex::ToHex as _;
use lb_core::codec::SerializeOp as _;
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use num_bigint::BigUint;
use rand::Rng as _;
use thiserror::Error;

const DEFAULT_FUNDS_PER_WALLET: u64 = 100;

/// Collection of wallet accounts that should be funded at genesis.
#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
pub struct WalletConfig {
    pub accounts: Vec<WalletAccount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WalletConfigError {
    #[error("wallet user count must be non-zero")]
    ZeroUsers,

    #[error(
        "wallet funds must allocate at least 1 token per user (total_funds={total_funds}, users={users})"
    )]
    InsufficientFunds { total_funds: u64, users: usize },

    #[error("wallet account value must be positive for '{label}'")]
    ZeroAccountValue { label: String },

    #[error("wallet config contains duplicate public key; duplicate at account '{label}'")]
    DuplicatePublicKey { label: String },

    #[error("wallet funds overflow (users={users}, funds_per_wallet={funds_per_wallet})")]
    FundsOverflow { users: usize, funds_per_wallet: u64 },
}

impl WalletConfig {
    #[must_use]
    pub const fn new(accounts: Vec<WalletAccount>) -> Self {
        Self { accounts }
    }

    pub fn uniform(total_funds: u64, users: NonZeroUsize) -> Result<Self, WalletConfigError> {
        let user_count = users.get();
        let user_count_u64 = user_count as u64;

        if total_funds < user_count_u64 {
            return Err(WalletConfigError::InsufficientFunds {
                total_funds,
                users: user_count,
            });
        }

        let base_allocation = total_funds / user_count_u64;
        let remainder = (total_funds % user_count_u64) as usize;

        let accounts = (0..user_count)
            .map(|idx| {
                WalletAccount::deterministic(
                    idx as u64,
                    allocation_for(idx, base_allocation, remainder),
                    false,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        let wallet = Self { accounts };
        wallet.validate(false, false)?;

        Ok(wallet)
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    #[must_use]
    pub const fn account_count(&self) -> usize {
        self.accounts.len()
    }

    pub fn validate(
        &self,
        allow_multiple_genesis_tokens_per_wallet: bool,
        allow_zero_value_genesis_tokens: bool,
    ) -> Result<(), WalletConfigError> {
        let mut seen_public_keys = HashSet::new();

        for account in &self.accounts {
            if !allow_zero_value_genesis_tokens {
                validate_account_value(account)?;
            }
            if !allow_multiple_genesis_tokens_per_wallet {
                ensure_unique_public_key(account, &mut seen_public_keys)?;
            }
        }

        Ok(())
    }
}

fn allocation_for(index: usize, base_allocation: u64, remainder: usize) -> u64 {
    base_allocation + u64::from(index < remainder)
}

pub fn wallet_config_for_users(users: usize) -> Result<WalletConfig, WalletConfigError> {
    let user_count = NonZeroUsize::new(users).ok_or(WalletConfigError::ZeroUsers)?;
    let total_funds = DEFAULT_FUNDS_PER_WALLET
        .checked_mul(user_count.get() as u64)
        .ok_or(WalletConfigError::FundsOverflow {
            users,
            funds_per_wallet: DEFAULT_FUNDS_PER_WALLET,
        })?;

    WalletConfig::uniform(total_funds, user_count)
}

/// Wallet account that may hold funds in the genesis state.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WalletAccount {
    pub label: String,
    pub secret_key: ZkKey,
    pub value: u64,
}

impl WalletAccount {
    pub fn new(
        label: String,
        secret_key: ZkKey,
        value: u64,
        allow_zero_value_genesis_tokens: bool,
    ) -> Result<Self, WalletConfigError> {
        if value == 0 && !allow_zero_value_genesis_tokens {
            return Err(WalletConfigError::ZeroAccountValue { label });
        }

        Ok(Self {
            label,
            secret_key,
            value,
        })
    }

    pub fn deterministic(
        index: u64,
        value: u64,
        allow_zero_value_genesis_tokens: bool,
    ) -> Result<Self, WalletConfigError> {
        let mut seed = [0u8; 32];
        seed[..2].copy_from_slice(b"wl");
        seed[2..10].copy_from_slice(&index.to_le_bytes());

        let secret_key = ZkKey::from(BigUint::from_bytes_le(&seed));
        Self::new(
            format!("wallet-user-{index}"),
            secret_key,
            value,
            allow_zero_value_genesis_tokens,
        )
    }

    pub fn random() -> Result<Self, WalletConfigError> {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill(&mut seed);

        let secret_key = ZkKey::from(BigUint::from_bytes_le(&seed));
        let index = u64::from_le_bytes(seed[..8].try_into().expect("seed has len 32"));
        Self::new(format!("wallet-r-user-{index}"), secret_key, 0, true)
    }

    #[must_use]
    pub fn public_key(&self) -> ZkPublicKey {
        self.secret_key.to_public_key()
    }

    #[must_use]
    pub fn public_key_hex(&self) -> String {
        self.secret_key
            .to_public_key()
            .to_bytes()
            .expect("is valid")
            .encode_hex()
    }
}

fn validate_account_value(account: &WalletAccount) -> Result<(), WalletConfigError> {
    if account.value == 0 {
        return Err(WalletConfigError::ZeroAccountValue {
            label: account.label.clone(),
        });
    }

    Ok(())
}

fn ensure_unique_public_key(
    account: &WalletAccount,
    seen_public_keys: &mut HashSet<ZkPublicKey>,
) -> Result<(), WalletConfigError> {
    if seen_public_keys.insert(account.public_key()) {
        return Ok(());
    }

    Err(WalletConfigError::DuplicatePublicKey {
        label: account.label.clone(),
    })
}
