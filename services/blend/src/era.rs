//! What the Blend services follow of the chain's eras: each runs an epoch
//! under the settings of the epoch's era.

use core::time::Duration;

use lb_cryptarchia_engine::era::{Era, EraSchedule, ScheduledEra};
use time::OffsetDateTime;

use crate::settings::TimingSettings;

/// Era `era` of a service's settings.
///
/// # Panics
///
/// If the schedule has no era `era`: every epoch's era is scheduled.
pub fn scheduled<Settings>(eras: &EraSchedule<Settings>, era: Era) -> &ScheduledEra<Settings> {
    eras.get(era).expect("every epoch's era is scheduled")
}

/// A service's settings in era `era`.
pub fn settings_in<Settings>(eras: &EraSchedule<Settings>, era: Era) -> &Settings {
    &scheduled(eras, era).entry.parameters
}

/// The epoch transition period of the era in force now by the wall clock, the
/// genesis era's before genesis: how long a service's epochs transition for.
///
/// It stays the same when a later era changes it. Following the change is left
/// to Blend's era-activated network behaviour.
pub fn epoch_transition_period_in_force(timings: &EraSchedule<TimingSettings>) -> Duration {
    timings
        .at_time(OffsetDateTime::now_utc())
        .unwrap_or_else(|| timings.genesis())
        .entry
        .parameters
        .epoch_transition_period
}

#[cfg(test)]
mod tests {
    use core::{num::NonZero, time::Duration};

    use lb_cryptarchia_engine::era::{EraEntriesAfterGenesis, EraEntry, EraSchedule};
    use time::OffsetDateTime;

    use super::epoch_transition_period_in_force;
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

    /// Two eras from `genesis_time`, whose epochs transition for 5 s and 7 s:
    /// era 1 starts at epoch 2, 20 s after genesis.
    fn two_eras(genesis_time: OffsetDateTime) -> EraSchedule<TimingSettings> {
        let entry = |epoch_transition| EraEntry {
            slot_duration: Duration::from_secs(1),
            epoch_length_in_slots: NonZero::new(10).unwrap(),
            transition_slots: 30,
            parameters: timing(epoch_transition),
        };
        EraSchedule::new(
            genesis_time,
            entry(Duration::from_secs(5)),
            EraEntriesAfterGenesis::from((NonZero::new(2).unwrap(), entry(Duration::from_secs(7)))),
        )
        .unwrap()
    }

    #[test]
    fn epochs_transition_for_the_period_of_the_era_in_force() {
        let started = two_eras(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(
            epoch_transition_period_in_force(&started),
            Duration::from_secs(7)
        );
    }

    #[test]
    fn before_genesis_epochs_transition_for_the_genesis_era_period() {
        let not_started = two_eras(OffsetDateTime::now_utc() + time::Duration::days(1));
        assert_eq!(
            epoch_transition_period_in_force(&not_started),
            Duration::from_secs(5)
        );
    }
}
