use std::cmp::Reverse;

use lb_core::mantle::{
    Note, Utxo,
    ledger::{Inputs, Outputs},
    ops::transfer::TransferOp,
};
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use lb_testing_framework::configs::wallet::WalletAccount;
use lb_wallet::WalletError;

pub struct WalletFundingResources {
    sender: WalletFundingSource,
    fee_sponsor: Option<WalletFundingSource>,
}

impl WalletFundingResources {
    #[must_use]
    pub const fn new(sender: WalletFundingSource) -> Self {
        Self {
            sender,
            fee_sponsor: None,
        }
    }

    #[must_use]
    pub fn from_sender(account: WalletAccount, available_utxos: Vec<Utxo>) -> Self {
        Self::new(WalletFundingSource::new(account, available_utxos))
    }

    #[must_use]
    pub const fn fee_sponsored(
        sender: WalletFundingSource,
        fee_sponsor: WalletFundingSource,
    ) -> Self {
        Self {
            sender,
            fee_sponsor: Some(fee_sponsor),
        }
    }

    #[must_use]
    pub fn with_fee_sponsor(self, fee_sponsor: WalletFundingSource) -> Self {
        Self {
            sender: self.sender,
            fee_sponsor: Some(fee_sponsor),
        }
    }

    #[must_use]
    pub const fn sender(&self) -> &WalletFundingSource {
        &self.sender
    }

    #[must_use]
    pub const fn policy(&self) -> WalletFundingPolicy {
        if self.fee_sponsor.is_some() {
            WalletFundingPolicy::FeeSponsored
        } else {
            WalletFundingPolicy::SenderPays
        }
    }

    #[must_use]
    pub const fn fee_sponsor(&self) -> Option<&WalletFundingSource> {
        self.fee_sponsor.as_ref()
    }

    pub(crate) fn into_parts(self) -> (WalletFundingSource, Option<WalletFundingSource>) {
        (self.sender, self.fee_sponsor)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum WalletFundingPolicy {
    SenderPays,
    FeeSponsored,
}

pub struct WalletFundingSource {
    account: WalletAccount,
    funding_utxos: WalletFundingUtxos,
}

impl WalletFundingSource {
    #[must_use]
    pub fn new(account: WalletAccount, available_utxos: Vec<Utxo>) -> Self {
        let change_pk = account.public_key();
        Self {
            account,
            funding_utxos: WalletFundingUtxos::new(change_pk, available_utxos),
        }
    }

    #[must_use]
    pub const fn public_key(&self) -> ZkPublicKey {
        self.funding_utxos.change_pk()
    }

    #[must_use]
    pub(crate) const fn signing_key(&self) -> &ZkKey {
        &self.account.secret_key
    }

    #[must_use]
    pub fn available_utxos(&self) -> &[Utxo] {
        self.funding_utxos.available_utxos()
    }

    #[must_use]
    pub(crate) fn into_funding_utxos(self) -> WalletFundingUtxos {
        self.funding_utxos
    }
}

pub struct WalletFundingUtxos {
    change_pk: ZkPublicKey,
    available_utxos: Vec<Utxo>,
}

impl WalletFundingUtxos {
    #[must_use]
    const fn new(change_pk: ZkPublicKey, available_utxos: Vec<Utxo>) -> Self {
        Self {
            change_pk,
            available_utxos,
        }
    }

    #[must_use]
    pub(crate) const fn change_pk(&self) -> ZkPublicKey {
        self.change_pk
    }

    #[must_use]
    pub(crate) fn available_utxos(&self) -> &[Utxo] {
        &self.available_utxos
    }

    #[must_use]
    pub(crate) fn into_available_utxos(self) -> Vec<Utxo> {
        self.available_utxos
    }
}

pub struct WalletSelectedInputs {
    inputs: Vec<Utxo>,
    total: u64,
}

impl WalletSelectedInputs {
    pub fn largest_first_covering(mut utxos: Vec<Utxo>, target: u64) -> Result<Self, WalletError> {
        if target == 0 {
            return Ok(Self {
                inputs: Vec::new(),
                total: 0,
            });
        }

        utxos.sort_by_key(|utxo| Reverse(utxo.note.value));

        let available = utxos.iter().map(|utxo| utxo.note.value).sum();
        let mut total = 0u64;
        let mut inputs = Vec::new();

        for utxo in utxos {
            total = total.saturating_add(utxo.note.value);
            inputs.push(utxo);
            if total >= target {
                return Ok(Self { inputs, total });
            }
        }

        Err(WalletError::InsufficientFunds { available })
    }

    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }

    #[must_use]
    pub fn into_utxos(self) -> Vec<Utxo> {
        self.inputs
    }
}

pub struct WalletFundedTransfer {
    transfer: TransferOp,
    selected_inputs: Vec<Utxo>,
}

impl WalletFundedTransfer {
    #[must_use]
    pub const fn transfer(&self) -> &TransferOp {
        &self.transfer
    }

    #[must_use]
    pub fn into_parts(self) -> (TransferOp, Vec<Utxo>) {
        (self.transfer, self.selected_inputs)
    }
}

pub fn build_wallet_funded_transfer(
    available_utxos: Vec<Utxo>,
    outputs: Vec<Note>,
    change_pk: ZkPublicKey,
) -> Result<WalletFundedTransfer, WalletError> {
    let output_total = outputs
        .iter()
        .fold(0u64, |total, output| total.saturating_add(output.value));
    let selected_inputs =
        WalletSelectedInputs::largest_first_covering(available_utxos, output_total)?;
    let change = selected_inputs.total() - output_total;
    let mut transfer_outputs = outputs;

    if change > 0 {
        transfer_outputs.push(Note::new(change, change_pk));
    }

    let selected_inputs = selected_inputs.into_utxos();
    let transfer = TransferOp {
        inputs: Inputs::new(selected_inputs.iter().map(Utxo::id).collect()),
        outputs: Outputs::new(transfer_outputs),
    };

    Ok(WalletFundedTransfer {
        transfer,
        selected_inputs,
    })
}

pub enum WalletFundingOutcome<T> {
    NeedsMoreInputs,
    Funded(T),
}

pub struct WalletFundingPlan {
    ordered_utxos: Vec<Utxo>,
    base_inputs: Vec<Utxo>,
}

impl WalletFundingPlan {
    #[must_use]
    pub fn largest_first(mut utxos: Vec<Utxo>, base_inputs: Vec<Utxo>) -> Self {
        utxos.sort_by_key(|utxo| Reverse(utxo.note.value));
        Self {
            ordered_utxos: utxos,
            base_inputs,
        }
    }

    #[must_use]
    pub fn smallest_first(mut utxos: Vec<Utxo>, base_inputs: Vec<Utxo>) -> Self {
        utxos.sort_by_key(|utxo| utxo.note.value);
        Self {
            ordered_utxos: utxos,
            base_inputs,
        }
    }

    pub fn fund_with<T, E>(
        &self,
        mut evaluate_inputs: impl FnMut(&[Utxo]) -> Result<WalletFundingOutcome<T>, E>,
    ) -> Result<T, E>
    where
        E: From<WalletError>,
    {
        for ordered_count in 0..=self.ordered_utxos.len() {
            let selected_inputs = self.inputs_with_ordered_prefix(ordered_count);

            match evaluate_inputs(&selected_inputs)? {
                WalletFundingOutcome::NeedsMoreInputs => {}
                WalletFundingOutcome::Funded(funded) => return Ok(funded),
            }
        }

        Err(WalletError::InsufficientFunds {
            available: self
                .base_inputs
                .iter()
                .chain(self.ordered_utxos.iter())
                .map(|utxo| utxo.note.value)
                .sum(),
        }
        .into())
    }

    fn inputs_with_ordered_prefix(&self, ordered_count: usize) -> Vec<Utxo> {
        let mut selected_inputs = Vec::with_capacity(self.base_inputs.len() + ordered_count);
        selected_inputs.extend_from_slice(&self.base_inputs);
        selected_inputs.extend_from_slice(&self.ordered_utxos[..ordered_count]);
        selected_inputs
    }
}

#[derive(Debug, Clone)]
pub struct WalletReservedInputs {
    sender: Vec<Utxo>,
    fee_sponsor: Vec<Utxo>,
}

impl WalletReservedInputs {
    #[must_use]
    pub const fn new(sender: Vec<Utxo>, fee_sponsor: Vec<Utxo>) -> Self {
        Self {
            sender,
            fee_sponsor,
        }
    }

    #[must_use]
    pub fn into_sender_and_fee_sponsor_inputs(self) -> (Vec<Utxo>, Vec<Utxo>) {
        (self.sender, self.fee_sponsor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utxo(value: u64, output_index: usize) -> Utxo {
        Utxo::new(
            [output_index as u8; 32],
            output_index,
            Note::new(value, ZkPublicKey::new(1u8.into())),
        )
    }

    fn account(index: u64) -> WalletAccount {
        WalletAccount::deterministic(index, 1, false).expect("test wallet account should build")
    }

    #[test]
    fn funding_resources_default_to_sender_pays_policy() {
        let resources = WalletFundingResources::from_sender(account(0), vec![utxo(10, 0)]);

        assert!(matches!(
            resources.policy(),
            WalletFundingPolicy::SenderPays
        ));
        assert!(resources.fee_sponsor().is_none());
    }

    #[test]
    fn funding_resources_can_use_explicit_fee_sponsor_policy() {
        let sender = WalletFundingSource::new(account(0), vec![utxo(10, 0)]);
        let fee_sponsor = WalletFundingSource::new(account(1), vec![utxo(20, 1)]);
        let resources = WalletFundingResources::fee_sponsored(sender, fee_sponsor);

        assert_eq!(resources.policy(), WalletFundingPolicy::FeeSponsored);
        assert_eq!(
            resources
                .fee_sponsor()
                .expect("fee sponsor should be present")
                .available_utxos()
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>(),
            vec![20]
        );
    }

    #[test]
    fn zero_target_selects_no_inputs() {
        let selected_inputs =
            WalletSelectedInputs::largest_first_covering(vec![utxo(20, 0), utxo(10, 1)], 0)
                .expect("zero target should be funded without inputs");

        assert_eq!(selected_inputs.total(), 0);
        assert!(selected_inputs.into_utxos().is_empty());
    }

    #[test]
    fn funding_plan_evaluates_base_inputs_before_extra_inputs() {
        let plan =
            WalletFundingPlan::smallest_first(vec![utxo(10, 1), utxo(20, 2)], vec![utxo(8, 0)]);
        let mut observed_input_values = Vec::new();

        let selected_count = plan
            .fund_with::<_, WalletError>(|selected_inputs| {
                observed_input_values.push(
                    selected_inputs
                        .iter()
                        .map(|utxo| utxo.note.value)
                        .collect::<Vec<_>>(),
                );

                if selected_inputs
                    .iter()
                    .map(|utxo| utxo.note.value)
                    .sum::<u64>()
                    >= 8
                {
                    Ok(WalletFundingOutcome::Funded(selected_inputs.len()))
                } else {
                    Ok(WalletFundingOutcome::NeedsMoreInputs)
                }
            })
            .expect("base inputs should already fund this plan");

        assert_eq!(selected_count, 1);
        assert_eq!(observed_input_values, vec![vec![8]]);
    }

    #[test]
    fn funding_plan_can_be_funded_without_inputs() {
        let plan = WalletFundingPlan::largest_first(vec![utxo(10, 0)], Vec::new());
        let mut observed_input_values = Vec::new();

        let selected_count = plan
            .fund_with::<_, WalletError>(|selected_inputs| {
                observed_input_values.push(
                    selected_inputs
                        .iter()
                        .map(|utxo| utxo.note.value)
                        .collect::<Vec<_>>(),
                );

                Ok(WalletFundingOutcome::Funded(selected_inputs.len()))
            })
            .expect("empty input set should be allowed when the evaluator accepts it");

        assert_eq!(selected_count, 0);
        assert_eq!(observed_input_values, vec![Vec::<u64>::new()]);
    }
}
