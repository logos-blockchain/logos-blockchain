use std::{num::NonZero, ops::Add, time::Duration};

#[cfg(feature = "serde")]
use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use time::OffsetDateTime;
#[cfg(feature = "tokio")]
use tokio::time::{Interval, MissedTickBehavior};

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, Eq, PartialEq, Copy, Hash, PartialOrd, Ord)]
pub struct Slot(u64);

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq, Copy, Hash, PartialOrd, Ord)]
pub struct Epoch(u32);

impl Epoch {
    #[must_use]
    pub const fn new(inner: u32) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl Slot {
    #[must_use]
    pub const fn new(inner: u64) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn to_le_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }

    #[must_use]
    pub const fn to_be_bytes(&self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    #[must_use]
    pub const fn genesis() -> Self {
        Self(0)
    }

    #[must_use]
    pub fn from_offset_and_config(
        offset_date_time: OffsetDateTime,
        slot_config: SlotConfig,
    ) -> Self {
        // TODO: leap seconds / weird time stuff
        let since_start = offset_date_time - slot_config.chain_start_time;
        if since_start.is_negative() {
            // current slot is behind the start time, so return default 0
            Self::genesis()
        } else {
            // since_start is already checked never negative in this case
            // division panics if `slot_duration` is less than a second.
            Self::from(
                (since_start.whole_seconds() as u64)
                    .checked_div(slot_config.slot_duration.as_secs())
                    .expect("slots tick should be at least a second"),
            )
        }
    }

    #[must_use]
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl From<u32> for Epoch {
    fn from(epoch: u32) -> Self {
        Self(epoch)
    }
}

impl From<Epoch> for u32 {
    fn from(epoch: Epoch) -> Self {
        epoch.0
    }
}

impl TryFrom<u64> for Epoch {
    type Error = <u64 as TryInto<u32>>::Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        value.try_into().map(Self)
    }
}

impl From<u64> for Slot {
    fn from(slot: u64) -> Self {
        Self(slot)
    }
}

impl From<Slot> for u64 {
    fn from(slot: Slot) -> Self {
        slot.0
    }
}

impl Add<u64> for Slot {
    type Output = Self;

    fn add(self, rhs: u64) -> Self::Output {
        Self(self.0 + rhs)
    }
}

impl Add<u32> for Epoch {
    type Output = Self;

    fn add(self, rhs: u32) -> Self::Output {
        Self(self.0 + rhs)
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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

#[cfg_attr(feature = "serde", cfg_eval::cfg_eval, serde_with::serde_as)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Copy, Clone, Debug)]
pub struct SlotConfig {
    #[cfg_attr(feature = "serde", serde_as(as = "MinimalBoundedDuration<1, SECOND>"))]
    pub slot_duration: Duration,
    /// Start of the first epoch
    pub chain_start_time: OffsetDateTime,
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
    pub fn current_slot(&self, now: OffsetDateTime) -> Slot {
        Slot::from_offset_and_config(now, self.config)
    }

    /// Ticks at the start of each slot, starting from the next slot
    #[must_use]
    pub fn slot_interval(&self, now: OffsetDateTime) -> Interval {
        let slot_duration = self.config.slot_duration;
        let next_slot_start = self.config.chain_start_time
            + slot_duration * u64::from(self.current_slot(now) + 1) as u32;
        let delay = next_slot_start - now;
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now()
                + Duration::try_from(delay).expect("could not set slot timer duration"),
            slot_duration,
        );
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        interval
    }
}
