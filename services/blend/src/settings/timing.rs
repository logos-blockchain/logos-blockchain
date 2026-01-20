use core::{num::NonZeroU64, time::Duration};

use serde::{Deserialize, Serialize};

use crate::epoch_info::EpochHandler;

#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimingSettings {
    /// `S`: length of a session in terms of expected rounds (on average).
    pub rounds_per_session: NonZeroU64,
    /// `|I|`: length of an interval in terms of rounds.
    pub rounds_per_interval: NonZeroU64,
    #[serde_as(
        as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, lb_utils::bounded_duration::SECOND>"
    )]
    /// Duration of a round.
    pub round_duration: Duration,
    pub rounds_per_observation_window: NonZeroU64,
    /// Session transition period in rounds.
    pub rounds_per_session_transition_period: NonZeroU64,
    pub epoch_transition_period_in_slots: NonZeroU64,
}

impl TimingSettings {
    #[must_use]
    pub const fn session_duration(&self) -> Duration {
        Duration::from_secs(self.rounds_per_session.get() * self.round_duration.as_secs())
    }

    #[must_use]
    pub fn intervals_per_session(&self) -> NonZeroU64 {
        NonZeroU64::try_from(self.rounds_per_session.get() / self.rounds_per_interval.get()).expect("Obtained `0` when calculating the number of intervals per session, which is not allowed.")
    }

    #[must_use]
    pub const fn session_transition_period(&self) -> Duration {
        Duration::from_secs(
            self.rounds_per_session_transition_period.get() * self.round_duration.as_secs(),
        )
    }

    pub const fn epoch_handler<ChainService, RuntimeServiceId>(
        &self,
        chain_service: ChainService,
    ) -> EpochHandler<ChainService, RuntimeServiceId> {
        EpochHandler::new(chain_service, self.epoch_transition_period_in_slots)
    }
}
