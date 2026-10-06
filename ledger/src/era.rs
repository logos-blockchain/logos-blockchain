//! How a ledger state crosses from one era into the next.

use std::borrow::Cow;

use lb_cryptarchia_engine::{
    Slot,
    era::{Era, ScheduledEra},
};

use crate::{Config, LedgerState, config::EraScheduledConfig};

#[expect(
    clippy::multiple_inherent_impl,
    reason = "grouping how a state crosses era boundaries separately from the main impl"
)]
// TODO: LedgerState will soon be converted into a versioned enum, so the
// functions below won't return `Self` as in a specific version, but would
// return the overall enum instead.
impl LedgerState {
    /// The state brought from the era of its own slot into the era of `slot`,
    /// across every era boundary in between.
    ///
    /// It returns `None` if the provided slot is past the slot the ledger state
    /// is tracking.
    pub(crate) fn migrate_to_future_slot(
        self,
        slot: Slot,
        eras: &EraScheduledConfig,
    ) -> Option<Self> {
        if slot < self.slot() {
            return None;
        }

        let from = eras.at_slot(self.slot()).era.into_inner();
        let into = eras.at_slot(slot).era.into_inner();
        let era_schedule_for = |era| {
            eras.get(Era::new(era))
                .expect("every era up to the one of a slot is scheduled")
        };

        let mut state = self;
        for era in from..into {
            state = state.into_next_era(era_schedule_for(era), era_schedule_for(era + 1));
        }
        Some(state)
    }

    /// The state as the era of `slot` holds it, borrowed when that is the era
    /// of its own slot.
    pub(crate) fn as_in_era_of_slot(
        &self,
        slot: Slot,
        eras: &EraScheduledConfig,
    ) -> Option<Cow<'_, Self>> {
        if eras.at_slot(self.slot()).era == eras.at_slot(slot).era {
            Some(Cow::Borrowed(self))
        } else {
            Some(Cow::Owned(self.clone().migrate_to_future_slot(slot, eras)?))
        }
    }

    /// The state at the end of era `previous`, brought into era `next`, the
    /// one after it.
    // TODO: This will return the versioned enum once LedgerState is converted into
    // one.
    fn into_next_era(self, previous: &ScheduledEra<Config>, next: &ScheduledEra<Config>) -> Self {
        // The ledger's version in an era is its config's.
        match (&previous.entry.parameters, &next.entry.parameters) {
            (Config::V1(_), Config::V1(_)) => {
                let Self {
                    block_number,
                    cryptarchia_ledger,
                    mantle_ledger,
                } = self;
                Self {
                    block_number,
                    cryptarchia_ledger,
                    mantle_ledger,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use lb_cryptarchia_engine::{Epoch, Slot};

    use crate::{
        LedgerState,
        config::{EraScheduledConfig, schedule},
        cryptarchia::tests::{config, utxo},
    };

    /// Two eras of version 1, the second from epoch 1.
    fn two_eras() -> EraScheduledConfig {
        schedule(config(), [(1, config())])
    }

    #[test]
    fn a_state_crosses_into_an_era_of_its_version_unchanged() {
        let eras = two_eras();
        let state = LedgerState::from_utxos([utxo()], &eras.genesis().entry.parameters);
        let next_era = eras.epoch_starting_slot(Epoch::new(1));

        assert_eq!(
            state.clone().migrate_to_future_slot(next_era, &eras),
            Some(state)
        );
    }

    #[test]
    fn a_state_is_borrowed_within_its_own_era() {
        let eras = two_eras();
        let state = LedgerState::from_utxos([utxo()], &eras.genesis().entry.parameters);
        let last_slot_of_first_era =
            Slot::new(eras.epoch_starting_slot(Epoch::new(1)).into_inner() - 1);

        assert!(matches!(
            state.as_in_era_of_slot(last_slot_of_first_era, &eras),
            Some(Cow::Borrowed(_))
        ));
        assert!(matches!(
            state.as_in_era_of_slot(eras.epoch_starting_slot(Epoch::new(1)), &eras),
            Some(Cow::Owned(_))
        ));
    }
}
