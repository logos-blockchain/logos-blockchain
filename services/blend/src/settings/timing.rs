use core::{
    num::{NonZeroU64, NonZeroU128},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct EraTimingSettings {
    /// `S`: length of an epoch in terms of rounds.
    pub rounds_per_epoch: NonZeroU64,
    pub round_duration_in_seconds: NonZeroU64,
    pub rounds_per_observation_window: NonZeroU128,
    /// `η`: the network absorption of one hop, the rounds a message spends
    /// crossing the network between two blend nodes.
    pub network_absorption_in_rounds: NonZeroU64,
    /// `T_H`: the rounds a handshake with a core node is given to complete.
    pub core_handshake_deadline_in_rounds: NonZeroU128,
    pub epoch_transition_period: Duration,
}

impl EraTimingSettings {
    /// How long `rounds` rounds last.
    #[must_use]
    pub const fn duration_of(&self, rounds: u64) -> Duration {
        Duration::from_secs(
            self.round_duration_in_seconds
                .get()
                .checked_mul(rounds)
                .expect("The duration of the rounds overflowed."),
        )
    }
}
