//! How a ledger state crosses from one era into the next.

use std::borrow::Cow;

use lb_cryptarchia_engine::{
    Slot,
    era::{EraVersion, EraSchedule, ScheduledEra},
};

use crate::{Config, LedgerState};

/// The ledger's own versions, which the global era versions map to: a global
/// version that changes nothing in the ledger maps to the ledger version that
/// came before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LedgerVersion {
    V1,
}

impl From<EraVersion> for LedgerVersion {
    fn from(version: EraVersion) -> Self {
        match version {
            EraVersion::V1 => Self::V1,
        }
    }
}

#[expect(
    clippy::multiple_inherent_impl,
    reason = "grouping how a state crosses era boundaries separately from the main impl"
)]
impl LedgerState {
    /// The state brought from the era of its own slot into the era of `slot`,
    /// across every era boundary in between.
    pub(crate) fn into_era_of(self, slot: Slot, eras: &EraSchedule<Config>) -> Self {
        let (from, into) = (eras.at_slot(self.slot()).era, eras.at_slot(slot).era);
        eras.iter()
            .zip(eras.iter().skip(1))
            .filter(|(_, next)| from < next.era && next.era <= into)
            .fold(self, |state, (previous, next)| {
                state.into_next_era(previous, next)
            })
    }

    /// The state as the era of `slot` holds it, borrowed when that is the era
    /// of its own slot.
    pub(crate) fn in_era_of(&self, slot: Slot, eras: &EraSchedule<Config>) -> Cow<'_, Self> {
        if eras.at_slot(self.slot()).era == eras.at_slot(slot).era {
            Cow::Borrowed(self)
        } else {
            Cow::Owned(self.clone().into_era_of(slot, eras))
        }
    }

    /// The state at the end of era `previous`, brought into era `next`, the
    /// one after it.
    fn into_next_era(self, previous: &ScheduledEra<Config>, next: &ScheduledEra<Config>) -> Self {
        match (
            LedgerVersion::from(previous.entry.version),
            LedgerVersion::from(next.entry.version),
        ) {
            // Within a version, an era changes only the values of the
            // parameters, which the ledger reads from the config of the era
            // whenever it uses them: every component carries over unchanged.
            // A new component must be named here, so that the version that
            // adds it says what it becomes at an era boundary.
            (LedgerVersion::V1, LedgerVersion::V1) => {
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

    use lb_cryptarchia_engine::{Epoch, Slot, era::EraSchedule};

    use crate::{
        Config, LedgerState,
        config::schedule,
        cryptarchia::tests::{config, utxo},
    };

    /// Two eras of version 1, the second from epoch 1.
    fn two_eras() -> EraSchedule<Config> {
        schedule(config(), [(1, config())])
    }

    #[test]
    fn a_state_crosses_into_an_era_of_its_version_unchanged() {
        let eras = two_eras();
        let state = LedgerState::from_utxos([utxo()], &eras);
        let next_era = eras.epoch_starting_slot(Epoch::new(1));

        assert_eq!(state.clone().into_era_of(next_era, &eras), state);
    }

    #[test]
    fn a_state_is_borrowed_within_its_own_era() {
        let eras = two_eras();
        let state = LedgerState::from_utxos([utxo()], &eras);
        let last_slot_of_first_era =
            Slot::new(eras.epoch_starting_slot(Epoch::new(1)).into_inner() - 1);

        assert!(matches!(
            state.in_era_of(last_slot_of_first_era, &eras),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            state.in_era_of(eras.epoch_starting_slot(Epoch::new(1)), &eras),
            Cow::Owned(_)
        ));
    }
}
