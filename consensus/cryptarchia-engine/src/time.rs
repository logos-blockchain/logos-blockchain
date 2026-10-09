use std::num::NonZero;

pub use lb_time::{
    Epoch, Slot,
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
