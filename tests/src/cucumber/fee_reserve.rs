use std::{
    collections::{HashMap, HashSet},
    num::NonZero,
};

use lb_core::mantle::{NoteId, Utxo};
use lb_testing_framework::configs::wallet::WalletAccount;
use thiserror::Error;

use crate::{
    common::wallet::{
        WalletFundingSource, WalletId, WalletSyncSourceId, WalletUtxos, wallet_id_for_sync_source,
    },
    cucumber::{error::StepError, wallet::WalletStateView},
};

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
    reservations: ScenarioFeeReservations,
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

    pub fn reserve_for_wallet(
        &mut self,
        wallet_name: impl Into<String>,
        group_key: impl Into<String>,
        utxos: Vec<Utxo>,
    ) {
        self.reservations
            .reserve_for_wallet(wallet_name, group_key, utxos);
    }

    pub fn clear_wallet_reservations(&mut self, wallet_name: &str) {
        self.reservations.clear_wallet(wallet_name);
    }

    #[must_use]
    pub fn wallet_name_for_group(group_key: &str) -> String {
        wallet_id_for_sync_source(
            SCENARIO_FEE_ACCOUNT_NAME,
            &WalletSyncSourceId::from(group_key),
        )
        .into_string()
    }

    #[must_use]
    pub fn owns_wallet_name(wallet_name: &str) -> bool {
        wallet_name.starts_with(SCENARIO_FEE_ACCOUNT_NAME)
    }

    #[must_use]
    pub fn reserved_wallet_count(&self) -> usize {
        self.reservations.wallet_count()
    }

    pub fn funding_source_for_group(
        &self,
        group_key: &str,
        available_utxos: &WalletUtxos,
    ) -> Result<Option<WalletFundingSource>, ScenarioFeeFundingError> {
        let Some(fee_wallet_account) = self.wallet_account.clone() else {
            return Ok(None);
        };

        let fee_wallet_name = Self::wallet_name_for_group(group_key);
        let available_fee_utxos = available_utxos
            .get(fee_wallet_name.as_str())
            .cloned()
            .ok_or_else(|| ScenarioFeeFundingError::MissingGroupedWallet {
                fee_wallet_name: fee_wallet_name.clone(),
            })?;

        Ok(Some(WalletFundingSource::new(
            fee_wallet_account,
            self.available_utxos_for_group(group_key, available_fee_utxos),
        )))
    }

    #[must_use]
    pub fn state_observation_for(
        &self,
        wallet_id: impl Into<WalletId>,
        on_chain_utxos: Vec<Utxo>,
    ) -> WalletStateView {
        let wallet_id = wallet_id.into();
        let group_key = fee_wallet_group_key(wallet_id.as_str()).unwrap_or_default();
        let reserved_utxos = self.reservations.reserved_utxos_for_group(group_key);

        WalletStateView::new(wallet_id, on_chain_utxos, reserved_utxos)
    }

    #[must_use]
    fn available_utxos_for_group(&self, group_key: &str, on_chain_utxos: Vec<Utxo>) -> Vec<Utxo> {
        self.reservations
            .available_utxos_for_group(group_key, on_chain_utxos)
    }
}

/// Scenario fee UTXOs reserved while grouped transactions are being planned.
#[derive(Debug, Default)]
struct ScenarioFeeReservations {
    reserved_by_wallet: HashMap<String, ScenarioFeeReservation>,
}

#[derive(Debug, Default)]
struct ScenarioFeeReservation {
    group_key: String,
    utxos: Vec<Utxo>,
}

impl ScenarioFeeReservations {
    fn reserve_for_wallet(
        &mut self,
        wallet_name: impl Into<String>,
        group_key: impl Into<String>,
        utxos: Vec<Utxo>,
    ) {
        let reservation = self
            .reserved_by_wallet
            .entry(wallet_name.into())
            .or_default();
        reservation.group_key = group_key.into();
        reservation.utxos.extend(utxos);
    }

    fn clear_wallet(&mut self, wallet_name: &str) {
        self.reserved_by_wallet.remove(wallet_name);
    }

    fn wallet_count(&self) -> usize {
        self.reserved_by_wallet.len()
    }

    fn available_utxos_for_group(&self, group_key: &str, on_chain_utxos: Vec<Utxo>) -> Vec<Utxo> {
        let reserved_note_ids = self.reserved_note_ids_for_group(group_key);

        on_chain_utxos
            .into_iter()
            .filter(|utxo| !reserved_note_ids.contains(&utxo.id()))
            .collect()
    }

    fn reserved_utxos_for_group(&self, group_key: &str) -> Vec<Utxo> {
        self.reserved_by_wallet
            .values()
            .filter(|reservation| reservation.group_key == group_key)
            .flat_map(|reservation| reservation.utxos.iter().copied())
            .collect()
    }

    fn reserved_note_ids_for_group(&self, group_key: &str) -> HashSet<NoteId> {
        self.reserved_by_wallet
            .values()
            .filter(|reservation| reservation.group_key == group_key)
            .flat_map(|reservation| reservation.utxos.iter().map(Utxo::id))
            .collect()
    }
}

fn fee_wallet_group_key(wallet_name: &str) -> Option<&str> {
    let suffix = wallet_name.strip_prefix(SCENARIO_FEE_ACCOUNT_NAME)?;
    match suffix {
        "" => Some(""),
        suffix => suffix.strip_prefix('@'),
    }
}

#[derive(Debug, Error)]
pub enum ScenarioFeeFundingError {
    #[error("scenario fee account state `{fee_wallet_name}` not found in grouped scan")]
    MissingGroupedWallet { fee_wallet_name: String },
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
