//! Stake inference diagnostics and epoch-transition reconstruction.
//!
//! These types describe the observed block density and the stake updates
//! derived from it. They are used by the ledger API and by chain-service to
//! correlate candidate computations with committed TSI transitions.

use lb_core::mantle::Value;
use lb_cryptarchia_engine::{Epoch, Slot};

use super::{LedgerState, stake::PRECISION};
use crate::Config;

/// Snapshot of the stake-inference inputs used by the current ledger state.
#[derive(Clone, Copy, Debug)]
pub struct TsiDiagnostic {
    pub measured_block_density: u64,
    pub expected_block_density: u64,
    pub inference_period: u64,
    pub learning_rate: f64,
}

/// One stake-inference update between two consecutive epochs.
#[derive(Clone, Copy, Debug)]
pub struct TsiEpochTransition {
    pub from_epoch: u32,
    pub to_epoch: u32,
    pub old_total_stake: Value,
    pub new_total_stake: Value,
    pub measured_block_density: u64,
}

/// Returns the current stake-inference inputs for diagnostic correlation.
pub(super) fn diagnostic(ledger_state: &LedgerState) -> TsiDiagnostic {
    TsiDiagnostic {
        measured_block_density: ledger_state.block_density.current_block_density(),
        expected_block_density: ledger_state.stake_inference.expected_block_density(),
        inference_period: ledger_state.stake_inference.period(),
        learning_rate: ledger_state.stake_inference.learning_rate(),
    }
}

/// Reconstructs the stake-inference transitions up to `to_epoch` lazily.
///
/// The first transition uses the density measured in the current epoch;
/// transitions for later epochs represent skipped epochs and use zero
/// density.
pub(super) fn epoch_transitions_for(
    ledger_state: &LedgerState,
    to_epoch: Epoch,
) -> impl Iterator<Item = TsiEpochTransition> + '_ {
    let from_epoch = u32::from(ledger_state.epoch_state.epoch);
    let to_epoch = u32::from(to_epoch);
    let mut next_from_epoch = (to_epoch > from_epoch).then_some(from_epoch);
    let mut old_total_stake = ledger_state.epoch_state.total_stake;
    let measured_block_density = ledger_state.block_density.current_block_density();

    std::iter::from_fn(move || {
        let current_from_epoch = next_from_epoch?;
        let measured_block_density = if current_from_epoch == from_epoch {
            measured_block_density
        } else {
            0
        };
        let new_total_stake = ledger_state
            .stake_inference
            .total_stake_inference::<PRECISION>(old_total_stake, measured_block_density);
        next_from_epoch = current_from_epoch
            .checked_add(1)
            .filter(|next_epoch| *next_epoch < to_epoch);
        let transition = TsiEpochTransition {
            from_epoch: current_from_epoch,
            to_epoch: current_from_epoch + 1,
            old_total_stake,
            new_total_stake,
            measured_block_density,
        };
        old_total_stake = new_total_stake;
        Some(transition)
    })
}

/// Logs the diagnostic update associated with one reconstructed transition.
pub(super) fn log_update(
    ledger_state: &LedgerState,
    config: &Config,
    transition: &TsiEpochTransition,
    slot: Slot,
) {
    tracing::debug!(
        diagnostic = "blend_tsi_outage",
        event = "tsi_update",
        from_epoch = transition.from_epoch,
        to_epoch = transition.to_epoch,
        slot = u64::from(slot),
        boundary_slot = u64::from(config.epoch_config.starting_slot(
            &transition.to_epoch.into(),
            config.consensus_config.base_period_length(),
        )),
        old_total_stake = transition.old_total_stake,
        new_total_stake = transition.new_total_stake,
        measured_block_density = transition.measured_block_density,
        expected_block_density = ledger_state.stake_inference.expected_block_density(),
        inference_period = ledger_state.stake_inference.period(),
        learning_rate = ledger_state.stake_inference.learning_rate(),
        "TSI diagnostic update"
    );
}

#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::UncleSlots;
    use lb_groth16::{AdditiveGroup as _, Fr};

    use super::*;

    fn ledger_state() -> LedgerState {
        let config = super::super::tests::config();
        let mut state = LedgerState::from_utxos([], &config, Fr::ZERO);
        state
            .block_density
            .mark_occupied_slots(Slot::from(1), &UncleSlots::default());
        state
    }

    #[test]
    fn tsi_epoch_transitions_are_empty_for_current_or_earlier_epochs() {
        let mut state = ledger_state();
        state.epoch_state.epoch = 2.into();

        assert_eq!(state.tsi_epoch_transitions_for(1.into()).count(), 0);
        assert_eq!(state.tsi_epoch_transitions_for(2.into()).count(), 0);
    }

    #[test]
    fn tsi_epoch_transition_uses_current_density_for_one_epoch() {
        let state = ledger_state();
        let mut transitions = state.tsi_epoch_transitions_for(1.into());

        let transition = transitions.next().expect("one transition");
        assert_eq!(transition.from_epoch, 0);
        assert_eq!(transition.to_epoch, 1);
        assert_eq!(transition.measured_block_density, 1);
        assert!(transitions.next().is_none());
    }

    #[test]
    fn tsi_epoch_transitions_use_zero_density_for_skipped_epochs() {
        let state = ledger_state();
        let transitions = state
            .tsi_epoch_transitions_for(3.into())
            .collect::<Vec<_>>();

        assert_eq!(transitions.len(), 3);
        assert_eq!(transitions[0].measured_block_density, 1);
        assert!(
            transitions[1..]
                .iter()
                .all(|transition| transition.measured_block_density == 0)
        );
    }

    #[test]
    fn tsi_epoch_transitions_chain_stake_values() {
        let state = ledger_state();
        let transitions = state
            .tsi_epoch_transitions_for(3.into())
            .collect::<Vec<_>>();

        assert!(
            transitions
                .windows(2)
                .all(|window| window[0].new_total_stake == window[1].old_total_stake)
        );
    }
}
