//! What the Blend services follow of the chain's eras: each runs an epoch
//! under the settings of the epoch's era.

use core::time::Duration;

use lb_chain_service::Epoch;
use lb_cryptarchia_engine::era::{Era, Eras, ScheduledEra};

use crate::settings::TimingSettings;

/// Era `era` of a service's settings.
///
/// # Panics
///
/// If the schedule has no era `era`: every epoch's era is scheduled.
pub fn scheduled<Settings>(eras: &Eras<Settings>, era: Era) -> &ScheduledEra<Settings> {
    eras.get(era).expect("every epoch's era is scheduled")
}

/// A service's settings in era `era`.
pub fn settings_in<Settings>(eras: &Eras<Settings>, era: Era) -> &Settings {
    &scheduled(eras, era).entry.parameters
}

/// How long the transition into `epoch`, an epoch of era `era`, lasts: the
/// epoch transition period of the era. An epoch that opens an era lasts at
/// least the era's own transition period too, during which the network keeps
/// accepting the protocols of the era before it.
pub fn transition_period<Settings>(
    eras: &Eras<Settings>,
    era: Era,
    epoch: Epoch,
    timing: impl FnOnce(&Settings) -> &TimingSettings,
) -> Duration {
    let scheduled = scheduled(eras, era);
    let epoch_transition = timing(&scheduled.entry.parameters).epoch_transition_period;
    if era == Era::GENESIS || epoch != scheduled.first_epoch {
        return epoch_transition;
    }
    let era_transition = scheduled
        .entry
        .slot_duration
        .saturating_mul(u32::try_from(scheduled.entry.transition_slots).unwrap_or(u32::MAX));
    epoch_transition.max(era_transition)
}

#[cfg(test)]
mod tests {
    use core::{num::NonZero, time::Duration};

    use lb_chain_service::Epoch;
    use lb_cryptarchia_engine::era::{Era, EraEntriesAfterGenesis, EraEntry, EraVersion, Eras};
    use time::OffsetDateTime;

    use super::transition_period;
    use crate::settings::TimingSettings;

    fn timing(epoch_transition_period: Duration) -> TimingSettings {
        TimingSettings {
            rounds_per_epoch: NonZero::new(10).unwrap(),
            round_duration_in_seconds: NonZero::new(1).unwrap(),
            rounds_per_observation_window: NonZero::new(10).unwrap(),
            network_absorption_in_rounds: NonZero::new(1).unwrap(),
            core_handshake_deadline_in_rounds: NonZero::new(1).unwrap(),
            epoch_transition_period,
        }
    }

    /// Era 1 starts at epoch 2: its slots last 2 s and its transition period
    /// 30 slots, 60 s.
    fn two_eras(era_1_epoch_transition: Duration) -> Eras<TimingSettings> {
        let entry = |slot_duration, epoch_transition| EraEntry {
            version: EraVersion::V1,
            slot_duration,
            epoch_length_in_slots: NonZero::new(10).unwrap(),
            transition_slots: 30,
            parameters: timing(epoch_transition),
        };
        Eras::new(
            OffsetDateTime::UNIX_EPOCH,
            entry(Duration::from_secs(1), Duration::from_secs(5)),
            EraEntriesAfterGenesis::from((
                NonZero::new(2).unwrap(),
                entry(Duration::from_secs(2), era_1_epoch_transition),
            )),
        )
        .unwrap()
    }

    fn period(eras: &Eras<TimingSettings>, era: u16, epoch: u32) -> Duration {
        transition_period(eras, Era::new(era), Epoch::new(epoch), |time| time)
    }

    #[test]
    fn the_epoch_that_opens_an_era_lasts_at_least_the_era_transition() {
        let eras = two_eras(Duration::from_secs(5));
        // The genesis era has no era before it to transition from.
        assert_eq!(period(&eras, 0, 0), Duration::from_secs(5));
        assert_eq!(period(&eras, 0, 1), Duration::from_secs(5));
        // Era 1's first epoch, then a later one.
        assert_eq!(period(&eras, 1, 2), Duration::from_secs(60));
        assert_eq!(period(&eras, 1, 3), Duration::from_secs(5));
    }

    #[test]
    fn a_longer_epoch_transition_outlasts_the_era_transition() {
        let eras = two_eras(Duration::from_secs(90));
        assert_eq!(period(&eras, 1, 2), Duration::from_secs(90));
    }
}
