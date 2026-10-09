use std::num::NonZero;

pub use lb_time::{
    Epoch, Slot, SlotConfig,
    era::{Era, EraEntry, EraEntryView, EraSchedule},
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpochConfig {
    // The stake distribution is always taken at the beginning of the previous epoch.
    // This parameters controls how many slots to wait for it to be stabilized
    // The value is computed as epoch_stake_distribution_stabilization * int(floor(k / f))
    pub epoch_stake_distribution_stabilization: NonZero<u8>,
    // This parameter controls how many slots we wait after the stake distribution
    // snapshot has stabilized to take the nonce snapshot.
    pub epoch_period_nonce_buffer: NonZero<u8>,
    // This parameter controls how many slots we wait for the nonce snapshot to be considered
    // stabilized
    pub epoch_period_nonce_stabilization: NonZero<u8>,
}

impl EpochConfig {
    #[must_use]
    pub const fn epoch_length(&self, base_period_length: NonZero<u64>) -> u64 {
        epoch_length(
            self.epoch_stake_distribution_stabilization,
            self.epoch_period_nonce_buffer,
            self.epoch_period_nonce_stabilization,
            base_period_length,
        )
    }

    #[must_use]
    pub fn epoch(&self, slot: Slot, base_period_length: NonZero<u64>) -> Epoch {
        (u64::from(slot) / self.epoch_length(base_period_length))
            .try_into()
            .expect("Epoch should build from a correct configuration")
    }

    #[must_use]
    pub fn starting_slot(&self, epoch: &Epoch, base_period_length: NonZero<u64>) -> Slot {
        Slot::from(u64::from(u32::from(*epoch)) * self.epoch_length(base_period_length))
    }

    #[must_use]
    pub fn last_slot(&self, epoch: Epoch, base_period_length: NonZero<u64>) -> Slot {
        Slot::from(u64::from(epoch.into_inner() + 1) * self.epoch_length(base_period_length) - 1)
    }
}

#[must_use]
pub const fn epoch_length(
    epoch_stake_distribution_stabilization: NonZero<u8>,
    epoch_period_nonce_buffer: NonZero<u8>,
    epoch_period_nonce_stabilization: NonZero<u8>,
    base_period_length: NonZero<u64>,
) -> u64 {
    ((epoch_stake_distribution_stabilization.get() as u64)
        .saturating_add(epoch_period_nonce_buffer.get() as u64)
        .saturating_add(epoch_period_nonce_stabilization.get() as u64))
    .saturating_mul(base_period_length.get())
}

#[cfg(feature = "tokio")]
#[derive(Clone, Debug)]
pub struct SlotTimer {
    config: SlotConfig,
}

#[cfg(feature = "tokio")]
impl SlotTimer {
    #[must_use]
    pub const fn new(config: SlotConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn current_slot(&self, now: time::OffsetDateTime) -> Slot {
        Slot::from_offset_and_config(now, self.config)
    }

    /// Ticks at the start of each slot, starting from the next slot
    #[must_use]
    pub fn slot_interval(&self, now: time::OffsetDateTime) -> tokio::time::Interval {
        let slot_duration = self.config.slot_duration;
        let next_slot_start = self.config.genesis_time
            + slot_duration * u64::from(self.current_slot(now).strict_add(1.into())) as u32;
        let delay = next_slot_start - now;
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now()
                + core::time::Duration::try_from(delay).expect("could not set slot timer duration"),
            slot_duration,
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval
    }
}
