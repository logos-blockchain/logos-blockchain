//! The eras of a chain, resolved into slots and wall-clock time.
//!
//! An era is a range of consecutive epochs governed by one set of parameters.
//! Eras are numbered from 0, the era that starts at genesis, and each one names
//! the version of its parameter set: the rules, the parameter layout and the
//! codecs it runs. Every era has its own slot duration and epoch length, so its
//! boundaries are resolved era by era: an era's first slot follows the previous
//! era's last epoch, measured in the previous era's epoch length, and its start
//! time follows the previous era's last slot, measured in the previous era's
//! slot duration.

use core::{iter::once, num::NonZero, time::Duration};

use lb_utils::{
    bounded::UpperBoundedBTreeMap,
    bounded_duration::{MinimalBoundedDuration, SECOND},
};
use serde::{Deserialize, Serialize};
use strum::{EnumIter, IntoEnumIterator as _};
use thiserror::Error;
use time::OffsetDateTime;

use crate::time::{Epoch, Slot};

/// An era, by its number: its position in its chain's era schedule, counting
/// from 0 for the era that starts at genesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Era(u16);

impl Era {
    /// Era 0, the era that starts at genesis.
    pub const GENESIS: Self = Self(0);

    #[must_use]
    pub const fn new(inner: u16) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> u16 {
        self.0
    }
}

/// The version of an era's parameter set: the rules, the parameter layout and
/// the codecs that go with them.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    EnumIter,
    serde::Serialize,
    serde::Deserialize,
)]
#[repr(u16)]
#[serde(try_from = "u16", into = "u16")]
pub enum EraVersion {
    V1 = 1,
}

impl EraVersion {
    #[must_use]
    pub const fn tag(&self) -> u16 {
        *self as u16
    }

    fn variants_iter() -> impl Iterator<Item = Self> {
        Self::iter()
    }
}

impl From<EraVersion> for u16 {
    fn from(version: EraVersion) -> Self {
        version.tag()
    }
}

/// A tag no version of this release carries.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown era version {0}")]
pub struct UnknownEraVersion(pub u16);

impl TryFrom<u16> for EraVersion {
    type Error = UnknownEraVersion;

    fn try_from(tag: u16) -> Result<Self, Self::Error> {
        Self::variants_iter()
            .find(|version| version.tag() == tag)
            .ok_or(UnknownEraVersion(tag))
    }
}

/// The most eras a chain can have after its genesis era: eras are numbered by
/// a `u16`, and the genesis era is era 0.
pub const MAX_ERAS_AFTER_GENESIS: usize = u16::MAX as usize;

/// The eras of a chain after its genesis era, as its schedule lists them, each
/// keyed by the epoch it starts at. The genesis era starts at epoch 0, so no
/// era after it can.
///
/// The map keeps the eras ordered by first epoch, each first epoch once, and at
/// most [`MAX_ERAS_AFTER_GENESIS`] of them.
pub type EraEntriesAfterGenesis<Parameters> =
    UpperBoundedBTreeMap<NonZero<u32>, EraEntry<Parameters>, MAX_ERAS_AFTER_GENESIS>;

/// An era as a schedule lists it: the version of its parameters, the length of
/// its slots and epochs, its transition period, and what it carries.
///
/// The genesis era starts at epoch 0, and every later era at the epoch it is
/// keyed by in [`EraEntriesAfterGenesis`].
#[serde_with::serde_as]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EraEntry<Parameters> {
    pub version: EraVersion,
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
    pub epoch_length_in_slots: NonZero<u64>,
    /// How many slots, from the era's first, the network keeps accepting the
    /// identifiers of the era before it: its protocol names and topics.
    pub transition_slots: u64,
    pub parameters: Parameters,
}

/// An era of a schedule, resolved: the era as the schedule lists it, its
/// number, the epoch it starts at, and where it starts in slots and in time,
/// which follow from every era before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduledEra<Parameters> {
    pub era: Era,
    pub first_epoch: Epoch,
    pub first_slot: Slot,
    pub start_time: OffsetDateTime,
    pub entry: EraEntry<Parameters>,
}

impl<Parameters> ScheduledEra<Parameters> {
    pub const fn genesis(start_time: OffsetDateTime, entry: EraEntry<Parameters>) -> Self {
        Self {
            era: Era::GENESIS,
            first_epoch: Epoch::genesis(),
            first_slot: Slot::genesis(),
            start_time,
            entry,
        }
    }

    /// The first slot and the start time of `next_epoch`, based on this era
    /// schedule and starting time. `None` on overflow.
    fn epoch_start_time(&self, epoch: Epoch) -> Option<(Slot, OffsetDateTime)> {
        let epochs = u64::from(
            epoch
                .into_inner()
                .checked_sub(self.first_epoch.into_inner())?,
        );
        let slots = epochs.checked_mul(self.entry.epoch_length_in_slots.get())?;
        let first_slot = Slot::new(self.first_slot.into_inner().checked_add(slots)?);
        let start_time = self
            .start_time
            .checked_add(span(self.entry.slot_duration, slots)?)?;
        Some((first_slot, start_time))
    }
}

/// `slots` slots of `slot_duration` each, as a span of time. `None` on
/// overflow.
fn span(slot_duration: Duration, slots: u64) -> Option<time::Duration> {
    let nanos = slot_duration.as_nanos().checked_mul(u128::from(slots))?;
    // `from_nanos_u128` panics past `Duration::MAX`.
    let span = (nanos <= Duration::MAX.as_nanos()).then(|| Duration::from_nanos_u128(nanos))?;
    time::Duration::try_from(span).ok()
}

/// The eras the network accepts at a slot: the era in force, and the era
/// before it while the transition period that opens the era in force lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EraInForce {
    pub era: Era,
    pub retiring: Option<Era>,
}

impl EraInForce {
    /// The eras the network accepts: the era in force, then the retiring era,
    /// if any.
    pub fn eras(self) -> impl Iterator<Item = Era> {
        once(self.era).chain(self.retiring)
    }
}

/// Why a schedule's eras cannot be resolved.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ErasError {
    #[error(
        "era {} runs version {next:?}, older than the version {previous:?} of the era before it",
        .era.into_inner()
    )]
    VersionGoesBack {
        era: Era,
        previous: EraVersion,
        next: EraVersion,
    },
    #[error("era {} starts beyond the slots or the time this node can represent", .0.into_inner())]
    Overflow(Era),
}

/// A chain's eras, each resolved against the ones before it.
///
/// Never empty, and the first era starts at genesis: at epoch 0, slot 0 and
/// the genesis time. Era `n` is the `n`-th entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EraSchedule<Parameters> {
    genesis: ScheduledEra<Parameters>,
    after_genesis: Vec<ScheduledEra<Parameters>>,
}

impl<Parameters> EraSchedule<Parameters> {
    /// Resolves the eras of a chain that starts at `genesis_time`: the genesis
    /// era, which starts at epoch 0, and the eras after it, keyed by the epoch
    /// each starts at.
    pub fn new(
        genesis_time: OffsetDateTime,
        genesis: EraEntry<Parameters>,
        after_genesis: EraEntriesAfterGenesis<Parameters>,
    ) -> Result<Self, ErasError> {
        let genesis_era = ScheduledEra::genesis(genesis_time, genesis);
        let mut scheduled: Vec<ScheduledEra<Parameters>> = Vec::with_capacity(after_genesis.len());
        for (first_epoch, entry) in after_genesis {
            let first_epoch = Epoch::new(first_epoch.get());
            let previous_era = scheduled.last().unwrap_or(&genesis_era);
            let era = Era::new(
                previous_era
                    .era
                    .into_inner()
                    .checked_add(1)
                    .expect("a chain has at most `MAX_ERAS_AFTER_GENESIS` eras after genesis"),
            );
            // A later era never runs an older rule set, so the state of a
            // chain only ever crosses into a version from the one before.
            if entry.version < previous_era.entry.version {
                return Err(ErasError::VersionGoesBack {
                    era,
                    previous: previous_era.entry.version,
                    next: entry.version,
                });
            }
            let (first_slot, start_time) = previous_era
                .epoch_start_time(first_epoch)
                .ok_or(ErasError::Overflow(era))?;
            scheduled.push(ScheduledEra {
                era,
                first_epoch,
                first_slot,
                start_time,
                entry,
            });
        }
        Ok(Self {
            genesis: genesis_era,
            after_genesis: scheduled,
        })
    }

    /// Era 0, the era that starts at genesis.
    #[must_use]
    pub const fn genesis(&self) -> &ScheduledEra<Parameters> {
        &self.genesis
    }

    /// Era `era`, if the schedule has it.
    #[must_use]
    pub fn get(&self, era: Era) -> Option<&ScheduledEra<Parameters>> {
        self.iter().nth(usize::from(era.into_inner()))
    }

    /// Every era, in schedule order.
    pub fn iter(&self) -> impl Iterator<Item = &ScheduledEra<Parameters>> {
        once(&self.genesis).chain(&self.after_genesis)
    }

    /// The same schedule, each era carrying what `f` makes of it instead of
    /// its parameters. Numbers, boundaries, versions and lengths are kept.
    pub fn map<MapFn, Mapped>(&self, mut map_fn: MapFn) -> EraSchedule<Mapped>
    where
        MapFn: FnMut(&ScheduledEra<Parameters>) -> Mapped,
    {
        let genesis = map_era(&self.genesis, map_fn(&self.genesis));
        let after_genesis = self
            .after_genesis
            .iter()
            .map(|era| map_era(era, map_fn(era)))
            .collect();
        EraSchedule {
            genesis,
            after_genesis,
        }
    }

    /// The era `slot` belongs to.
    #[must_use]
    pub fn at_slot(&self, slot: Slot) -> &ScheduledEra<Parameters> {
        self.last_started(|era| era.first_slot <= slot)
            .expect("At least genesis era fulfils this predicate.")
    }

    pub fn at_time(&self, time: OffsetDateTime) -> Option<&ScheduledEra<Parameters>> {
        let slot = self.slot_at(time)?;
        Some(self.at_slot(slot))
    }

    pub fn elapsed_slots_since_era_start(&self, slot: Slot) -> u64 {
        let era_scheduled = self.at_slot(slot);
        slot.into_inner()
            .strict_sub(era_scheduled.first_slot.into_inner())
    }

    /// The eras the network accepts at `slot`: the era of `slot`, and the era
    /// before it if `slot` is within the transition period of its era.
    #[must_use]
    pub fn in_force_at_slot(&self, slot: Slot) -> EraInForce {
        let era_scheduled = self.at_slot(slot);
        let elapsed_slots_since_era_start = self.elapsed_slots_since_era_start(slot);
        let retiring_era = era_scheduled
            .era
            .into_inner()
            .checked_sub(1)
            // If current is era 0, `checked_sub(1)` will return `None`, so filter will return
            // `None`. Else, we check the previous era for transition.
            .filter(|_| elapsed_slots_since_era_start < era_scheduled.entry.transition_slots)
            .map(Era::new);
        EraInForce {
            era: era_scheduled.era,
            retiring: retiring_era,
        }
    }

    /// The era `epoch` belongs to.
    #[must_use]
    pub fn at_epoch(&self, epoch: Epoch) -> &ScheduledEra<Parameters> {
        self.last_started(|era| era.first_epoch <= epoch)
            .expect("At least genesis era fulfils this predicate.")
    }

    pub fn elapsed_epochs_since_era_start(&self, epoch: Epoch) -> u64 {
        let era_scheduled = self.at_epoch(epoch);
        epoch
            .into_inner()
            .strict_sub(era_scheduled.first_epoch.into_inner())
            .into()
    }

    /// The first slot of `epoch`, counted in the epoch length of its era.
    ///
    /// # Panics
    ///
    /// If the slot does not fit a [`Slot`].
    #[must_use]
    pub fn epoch_starting_slot(&self, epoch: Epoch) -> Slot {
        let era_at_epoch = self.at_epoch(epoch);
        let elapsed_era_epochs = self.elapsed_epochs_since_era_start(epoch);
        let elapsed_era_slots =
            elapsed_era_epochs.strict_mul(era_at_epoch.entry.epoch_length_in_slots.get());
        Slot::new(
            era_at_epoch
                .first_slot
                .into_inner()
                .strict_add(elapsed_era_slots),
        )
    }

    /// The epoch `slot` belongs to, counted in the epoch length of its era.
    ///
    /// # Panics
    ///
    /// If the epoch does not fit an [`Epoch`].
    #[must_use]
    pub fn epoch_of(&self, slot: Slot) -> Epoch {
        let era_at_slot = self.at_slot(slot);
        let elapsed_era_epochs = slot
            .into_inner()
            .strict_sub(era_at_slot.first_slot.into_inner())
            / era_at_slot.entry.epoch_length_in_slots.get();
        let epoch = u64::from(era_at_slot.first_epoch.into_inner()).strict_add(elapsed_era_epochs);
        Epoch::new(u32::try_from(epoch).expect("the epoch of a slot must fit an epoch number"))
    }

    /// The slot in progress at `time`, counted in the slot duration of its
    /// era: `None` if the time predates the genesis start time.
    ///
    /// # Panics
    ///
    /// If the slot does not fit a [`Slot`].
    #[must_use]
    pub fn slot_at(&self, time: OffsetDateTime) -> Option<Slot> {
        let era_at_time = self.last_started(|era| era.start_time <= time)?;
        let since_start = u128::try_from((time - era_at_time.start_time).whole_nanoseconds())
            .expect("Non-negative time delta when genesis era starts after the provided time.");
        let elapsed_era_slots =
            u64::try_from(since_start / era_at_time.entry.slot_duration.as_nanos())
                .expect("the slot in progress must fit a slot number");
        Some(Slot::new(
            era_at_time
                .first_slot
                .into_inner()
                .strict_add(elapsed_era_slots),
        ))
    }

    /// When `slot` starts, counted in the slot duration of its era.
    ///
    /// # Panics
    ///
    /// If the time does not fit an [`OffsetDateTime`].
    #[must_use]
    pub fn time_of(&self, slot: Slot) -> OffsetDateTime {
        let era_at_slot = self.at_slot(slot);
        let slots_into_era = self.elapsed_slots_since_era_start(slot);
        span(era_at_slot.entry.slot_duration, slots_into_era)
            .and_then(|span| era_at_slot.start_time.checked_add(span))
            .expect("the start of a slot must fit a date and time")
    }

    /// The last era `started` holds for, given that it holds for the eras up
    /// to some point of the schedule and for none after it.
    fn last_started<StartedFn>(&self, mut started: StartedFn) -> Option<&ScheduledEra<Parameters>>
    where
        StartedFn: FnMut(&ScheduledEra<Parameters>) -> bool,
    {
        if !started(&self.genesis) {
            return None;
        }
        match self.after_genesis.partition_point(started) {
            0 => Some(&self.genesis),
            started_after_genesis => Some(&self.after_genesis[started_after_genesis - 1]),
        }
    }
}

const fn map_era<Parameters, Mapped>(
    ScheduledEra {
        entry:
            EraEntry {
                epoch_length_in_slots,
                slot_duration,
                transition_slots,
                version,
                ..
            },
        era,
        first_epoch,
        first_slot,
        start_time,
    }: &ScheduledEra<Parameters>,
    parameters: Mapped,
) -> ScheduledEra<Mapped> {
    ScheduledEra {
        era: *era,
        first_epoch: *first_epoch,
        first_slot: *first_slot,
        start_time: *start_time,
        entry: EraEntry {
            version: *version,
            slot_duration: *slot_duration,
            epoch_length_in_slots: *epoch_length_in_slots,
            transition_slots: *transition_slots,
            parameters,
        },
    }
}

#[cfg(test)]
mod tests {
    use core::{num::NonZero, time::Duration};

    use time::OffsetDateTime;

    use super::{
        Era, EraEntriesAfterGenesis, EraEntry, EraInForce, EraSchedule, EraVersion, ErasError,
    };
    use crate::time::{Epoch, Slot};

    const GENESIS: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn entry(slot_duration: Duration, epoch_length: u64) -> EraEntry<()> {
        EraEntry {
            version: EraVersion::V1,
            slot_duration,
            epoch_length_in_slots: NonZero::new(epoch_length).unwrap(),
            transition_slots: 10,
            parameters: (),
        }
    }

    /// The eras of a chain starting with `genesis`, then each era of
    /// `after_genesis` from the epoch it is paired with.
    fn era_schedule<const AFTER_GENESIS: usize>(
        genesis: EraEntry<()>,
        after_genesis: [(u32, EraEntry<()>); AFTER_GENESIS],
    ) -> Result<EraSchedule<()>, ErasError> {
        let after_genesis =
            after_genesis.map(|(first_epoch, entry)| (NonZero::new(first_epoch).unwrap(), entry));
        EraSchedule::new(
            GENESIS,
            genesis,
            EraEntriesAfterGenesis::try_from_iter(after_genesis).unwrap(),
        )
    }

    /// Era 0: slots of 1 s, epochs of 100 slots. Era 1 from epoch 3: slots of
    /// 2 s, epochs of 50 slots. Era 2 from epoch 5: slots of 1.5 s, epochs of
    /// 3 slots. Era 3 from epoch 6: slots of 1 s, epochs of 100 slots.
    fn four_eras() -> EraSchedule<()> {
        era_schedule(
            entry(Duration::from_secs(1), 100),
            [
                (3, entry(Duration::from_secs(2), 50)),
                (5, entry(Duration::from_millis(1500), 3)),
                (6, entry(Duration::from_secs(1), 100)),
            ],
        )
        .unwrap()
    }

    fn seconds(seconds: f64) -> OffsetDateTime {
        GENESIS + time::Duration::seconds_f64(seconds)
    }

    #[test]
    fn each_era_starts_where_the_previous_one_ends_in_its_own_units() {
        let eras = four_eras();
        let boundaries: Vec<_> = [0, 299, 300, 399, 400, 402, 403]
            .map(|slot| {
                let era = eras.at_slot(Slot::new(slot));
                (slot, era.era, era.first_slot, era.start_time)
            })
            .into_iter()
            .collect();
        assert_eq!(
            boundaries,
            [
                (0, Era::GENESIS, Slot::genesis(), GENESIS),
                (299, Era::GENESIS, Slot::genesis(), GENESIS),
                // 3 epochs of 100 slots of 1 s.
                (300, Era::new(1), Slot::new(300), seconds(300.0)),
                (399, Era::new(1), Slot::new(300), seconds(300.0)),
                // Then 2 epochs of 50 slots of 2 s.
                (400, Era::new(2), Slot::new(400), seconds(500.0)),
                (402, Era::new(2), Slot::new(400), seconds(500.0)),
                // Then 1 epoch of 3 slots of 1.5 s.
                (403, Era::new(3), Slot::new(403), seconds(504.5)),
            ]
        );
    }

    #[test]
    fn slots_and_epochs_are_counted_in_the_units_of_their_era() {
        let eras = four_eras();
        let epochs = [0, 99, 100, 299, 300, 349, 350, 399, 400, 402, 403, 502, 503]
            .map(|slot| (slot, eras.epoch_of(Slot::new(slot)).into_inner()));
        assert_eq!(
            epochs,
            [
                (0, 0),
                (99, 0),
                (100, 1),
                (299, 2),
                (300, 3),
                (349, 3),
                (350, 4),
                (399, 4),
                (400, 5),
                (402, 5),
                (403, 6),
                (502, 6),
                (503, 7),
            ]
        );

        // Before genesis, no slot is in progress.
        assert_eq!(eras.slot_at(seconds(-10.0)), None);
        let slots = [
            0.0, 0.999, 1.0, 299.999, 300.0, 301.999, 302.0, 500.0, 501.499, 501.5, 504.5,
        ]
        .map(|at| (at, eras.slot_at(seconds(at)).unwrap().into_inner()));
        assert_eq!(
            slots,
            [
                (0.0, 0),
                (0.999, 0),
                (1.0, 1),
                (299.999, 299),
                (300.0, 300),
                (301.999, 300),
                (302.0, 301),
                (500.0, 400),
                (501.499, 400),
                (501.5, 401),
                (504.5, 403),
            ]
        );
    }

    #[test]
    fn every_epoch_starts_where_its_era_lays_it_out() {
        let eras = four_eras();
        let starts = [0, 1, 2, 3, 4, 5, 6, 7].map(|epoch| {
            (
                epoch,
                eras.epoch_starting_slot(Epoch::new(epoch)).into_inner(),
            )
        });
        assert_eq!(
            starts,
            [
                (0, 0),
                (1, 100),
                (2, 200),
                // Era 1: epochs of 50 slots.
                (3, 300),
                (4, 350),
                // Era 2: epochs of 3 slots.
                (5, 400),
                // Era 3: epochs of 100 slots.
                (6, 403),
                (7, 503),
            ]
        );
        for epoch in (0..=7).map(Epoch::new) {
            let start = eras.epoch_starting_slot(epoch);
            assert_eq!(eras.epoch_of(start), epoch);
            assert_eq!(eras.at_epoch(epoch).era, eras.at_slot(start).era);
        }
    }

    #[test]
    fn every_slot_starts_when_the_previous_one_ends() {
        let eras = four_eras();
        for slot in (0..=600).map(Slot::new) {
            let start = eras.time_of(slot);
            assert_eq!(eras.slot_at(start), Some(slot));
            let next_start = eras.time_of(Slot::new(slot.into_inner() + 1));
            assert_eq!(
                eras.slot_at(next_start - time::Duration::NANOSECOND),
                Some(slot)
            );
        }
    }

    #[test]
    fn the_era_before_is_accepted_during_the_transition_period() {
        let eras = four_eras();
        let in_force = [0, 299, 300, 309, 310, 400, 402, 403, 412, 413]
            .map(|slot| (slot, eras.in_force_at_slot(Slot::new(slot))));
        let era = |era, retiring: Option<u16>| EraInForce {
            era: Era::new(era),
            retiring: retiring.map(Era::new),
        };
        assert_eq!(
            in_force,
            [
                // The genesis era has no era before it.
                (0, era(0, None)),
                (299, era(0, None)),
                // Each era's first 10 slots.
                (300, era(1, Some(0))),
                (309, era(1, Some(0))),
                (310, era(1, None)),
                (400, era(2, Some(1))),
                // Era 2 ends within its transition period: the next era retires
                // era 2, never era 1.
                (402, era(2, Some(1))),
                (403, era(3, Some(2))),
                (412, era(3, Some(2))),
                (413, era(3, None)),
            ]
        );
    }

    #[test]
    fn eras_are_found_by_number() {
        let eras = four_eras();
        for era in eras.iter() {
            assert_eq!(eras.get(era.era), Some(era));
        }
        assert_eq!(eras.get(Era::new(4)), None);
    }

    #[test]
    fn a_mapped_schedule_keeps_its_boundaries() {
        let eras = four_eras();
        let numbers = eras.map(|era| era.era);
        for slot in [0, 300, 400, 403].map(Slot::new) {
            let era = numbers.at_slot(slot);
            assert_eq!(era.entry.parameters, era.era);
            assert_eq!(era.first_slot, eras.at_slot(slot).first_slot);
            assert_eq!(era.start_time, eras.at_slot(slot).start_time);
        }
    }

    #[test]
    fn eras_are_resolved_in_epoch_order_whatever_order_they_are_listed_in() {
        let listed_backwards = era_schedule(
            entry(Duration::from_secs(1), 100),
            [
                (6, entry(Duration::from_secs(1), 100)),
                (5, entry(Duration::from_millis(1500), 3)),
                (3, entry(Duration::from_secs(2), 50)),
            ],
        )
        .unwrap();

        assert_eq!(listed_backwards, four_eras());
    }

    #[test]
    fn an_era_starting_past_the_last_slot_is_rejected() {
        let second = Duration::from_secs(1);
        // Two epochs of the genesis era already run past the last slot.
        assert_eq!(
            era_schedule(entry(second, u64::MAX), [(2, entry(second, 100))]),
            Err(ErasError::Overflow(Era::new(1)))
        );
    }

    #[test]
    fn an_era_starting_past_the_longest_duration_is_rejected() {
        // One epoch of 2^63 slots of 3 s fits the slots, but lasts longer than
        // a `Duration` can hold.
        assert_eq!(
            era_schedule(
                entry(Duration::from_secs(3), 1 << 63),
                [(1, entry(Duration::from_secs(1), 100))]
            ),
            Err(ErasError::Overflow(Era::new(1)))
        );
    }
}
