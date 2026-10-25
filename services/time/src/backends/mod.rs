mod common;
#[cfg(feature = "ntp")]
pub mod ntp;
pub mod system_time;

use lb_time::era::EraTimingSchedule;
#[cfg(feature = "ntp")]
pub use ntp::{NtpTimeBackend, NtpTimeBackendSettings};
pub use system_time::SystemTimeBackend;

use crate::{EpochSlotTickStream, SlotTick};

/// Abstraction over slot ticking systems
pub trait TimeBackend {
    type Settings;

    fn init(settings: Self::Settings, era_schedule: EraTimingSchedule) -> Self;

    /// Returns the current [`SlotTick`] and a stream of future [`SlotTick`]s
    /// that ticks at the start of each slot, starting from the next slot.
    fn tick_stream(self) -> (SlotTick, EpochSlotTickStream);
}
