//! Test-only helpers shared by the crate's tests.

use std::{num::NonZero, time::Duration};

use lb_cryptarchia_engine::EraSchedule;
use lb_time::era::{EpochLength, EraEntriesAfterGenesis, EraTiming};
use time::OffsetDateTime;

use crate::config::{Config, EraScheduledConfig};

/// A schedule running `genesis` from genesis, then each config of
/// `after_genesis` from the epoch it is paired with.
///
/// The ledger reads neither the slot durations nor the genesis time, so they
/// are left at 1 s and the Unix epoch.
#[must_use]
pub fn schedule(
    genesis: Config,
    after_genesis: impl IntoIterator<Item = (u32, Config)>,
) -> EraScheduledConfig {
    let timing = |config: &Config| EraTiming {
        slot_duration: Duration::from_secs(1),
        epoch_length: EpochLength::epoch_length(config),
    };
    let after_genesis = after_genesis.into_iter().collect::<Vec<_>>();
    let timings_after_genesis =
        EraEntriesAfterGenesis::try_from_iter(after_genesis.iter().map(|(first_epoch, config)| {
            (
                NonZero::new(*first_epoch).expect("an era after genesis starts after epoch 0"),
                timing(config),
            )
        }))
        .expect("each epoch starts one era at most");
    let configs = std::iter::once(genesis)
        .chain(after_genesis.into_iter().map(|(_, config)| config))
        .collect::<Vec<_>>();
    EraSchedule::new(
        OffsetDateTime::UNIX_EPOCH,
        timing(&configs[0]),
        timings_after_genesis,
    )
    .expect("eras starting within the last slot resolve")
    .map(|era| configs[usize::from(era.era.into_inner())].clone())
}

/// A schedule running `config` alone, from genesis.
#[must_use]
pub fn single_era(config: Config) -> EraScheduledConfig {
    schedule(config, [])
}
