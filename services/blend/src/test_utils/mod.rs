pub mod crypto;
pub mod dispatcher;
pub mod epoch;
pub mod membership;
pub mod mocks;

mod libp2p;
use core::{num::NonZero, time::Duration};

use lb_cryptarchia_engine::era::{EraEntriesAfterGenesis, EraEntry, EraVersion, Eras};
use time::OffsetDateTime;

pub use self::libp2p::*;

/// A schedule of a single era, the genesis era, under `settings`.
pub fn single_era<Settings>(settings: Settings) -> Eras<Settings> {
    Eras::new(
        OffsetDateTime::UNIX_EPOCH,
        EraEntry {
            version: EraVersion::V1,
            slot_duration: Duration::from_secs(1),
            epoch_length_in_slots: NonZero::new(100).expect("an epoch has slots"),
            transition_slots: 0,
            parameters: settings,
        },
        EraEntriesAfterGenesis::empty(),
    )
    .expect("a single era from genesis is a valid schedule")
}
