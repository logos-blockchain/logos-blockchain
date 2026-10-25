//! Test-only helpers shared by the crate's tests.

use std::{
    num::{NonZero, NonZeroU64},
    time::Duration,
};

use lb_time::era::{EraEntriesAfterGenesis, EraSchedule, EraTiming};
use time::OffsetDateTime;

use crate::{PoWEraSettings, service::ServiceEraSchedule};

/// The number of slots in an epoch of the schedules [`era_schedule`] builds.
pub const SLOTS_PER_EPOCH: u64 = 100;

/// A schedule with an era per reward window of `reward_windows`, era `i`
/// starting at epoch `i` with the reward window `reward_windows[i]`, rewards
/// enabled.
pub fn era_schedule(reward_windows: &[u64]) -> ServiceEraSchedule {
    let timing = EraTiming {
        slot_duration: Duration::from_secs(1),
        epoch_length: NonZero::new(SLOTS_PER_EPOCH).unwrap(),
    };
    let after_genesis =
        EraEntriesAfterGenesis::try_from_iter((1..reward_windows.len()).map(|first_epoch| {
            let first_epoch = NonZero::new(u32::try_from(first_epoch).unwrap()).unwrap();
            (first_epoch, timing)
        }))
        .unwrap();
    EraSchedule::new(OffsetDateTime::UNIX_EPOCH, timing, after_genesis)
        .unwrap()
        .map(|era| PoWEraSettings {
            slot_window: NonZeroU64::new(reward_windows[usize::from(era.era.into_inner())])
                .unwrap(),
            rewards_enabled: true,
        })
}
