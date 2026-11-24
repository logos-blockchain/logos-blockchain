//! Era schedules for the tests.

use core::{num::NonZero, time::Duration};

use lb_cryptarchia_engine::EraSchedule;
use lb_time::era::{EraEntriesAfterGenesis, EraTiming};
use time::OffsetDateTime;

/// `parameters` as the only era, from genesis.
pub fn single_era<Parameters: Clone>(parameters: Parameters) -> EraSchedule<Parameters> {
    schedule(parameters, [])
}

/// `genesis` from genesis, then each of `after_genesis` from the epoch it is
/// paired with, every epoch holding 10 slots of a second.
pub fn schedule<Parameters: Clone>(
    genesis: Parameters,
    after_genesis: impl IntoIterator<Item = (u32, Parameters)>,
) -> EraSchedule<Parameters> {
    let timing = EraTiming {
        slot_duration: Duration::from_secs(1),
        epoch_length: NonZero::new(10).unwrap(),
    };
    let after_genesis = after_genesis.into_iter().collect::<Vec<_>>();
    let timings_after_genesis = EraEntriesAfterGenesis::try_from_iter(
        after_genesis
            .iter()
            .map(|(first_epoch, _)| (NonZero::new(*first_epoch).unwrap(), timing)),
    )
    .unwrap();
    let parameters = core::iter::once(genesis)
        .chain(after_genesis.into_iter().map(|(_, parameters)| parameters))
        .collect::<Vec<_>>();
    EraSchedule::new(OffsetDateTime::UNIX_EPOCH, timing, timings_after_genesis)
        .unwrap()
        .map(|era| parameters[usize::from(era.era.into_inner())].clone())
}
