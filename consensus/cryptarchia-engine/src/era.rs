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

use core::{num::NonZero, time::Duration};

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
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

/// An era as a schedule lists it: the epoch it starts at, the version of its
/// parameters, the length of its slots and epochs, its transition period, and
/// what it carries.
#[serde_with::serde_as]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EraEntry<Parameters> {
    pub first_epoch: Epoch,
    pub version: EraVersion,
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
    pub epoch_length: NonZero<u64>,
    /// How many slots, from the era's first, the network keeps accepting the
    /// identifiers of the era before it: its protocol names and topics.
    pub transition_slots: u64,
    pub parameters: Parameters,
}

/// An era of a schedule, resolved: the era as the schedule lists it, its
/// number, and where it starts in slots and in time, which follow from every
/// era before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledEra<Parameters> {
    pub era: Era,
    pub first_slot: Slot,
    pub start_time: OffsetDateTime,
    pub entry: EraEntry<Parameters>,
}

impl<Parameters> ScheduledEra<Parameters> {
    /// The first slot and the start time of an era starting at `next_epoch`,
    /// right after this one. `None` on overflow.
    fn boundary(&self, next_epoch: Epoch) -> Option<(Slot, OffsetDateTime)> {
        let epochs = u64::from(
            next_epoch
                .into_inner()
                .checked_sub(self.entry.first_epoch.into_inner())?,
        );
        let slots = epochs.checked_mul(self.entry.epoch_length.get())?;
        let first_slot = Slot::new(self.first_slot.into_inner().checked_add(slots)?);
        let start_time = self
            .start_time
            .checked_add(span(self.entry.slot_duration, slots)?)?;
        Some((first_slot, start_time))
    }
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
        core::iter::once(self.era).chain(self.retiring)
    }
}

/// Why a list of eras cannot be resolved.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ErasError {
    #[error("a chain needs at least one era")]
    Empty,
    #[error("the first era must start at epoch 0, not at epoch {}", .0.into_inner())]
    FirstEraAfterGenesis(Epoch),
    #[error(
        "eras must start at strictly increasing epochs, but epoch {} follows epoch {}",
        .next.into_inner(),
        .previous.into_inner()
    )]
    OutOfOrder { previous: Epoch, next: Epoch },
    #[error("a chain has at most {} eras", u32::from(u16::MAX) + 1)]
    TooManyEras,
    #[error("era {} starts beyond the slots or the time this node can represent", .0.into_inner())]
    Overflow(Era),
}

/// A chain's eras, each resolved against the ones before it.
///
/// Never empty, and the first era starts at genesis: at epoch 0, slot 0 and
/// the genesis time. Era `n` is the `n`-th entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eras<Parameters> {
    genesis: ScheduledEra<Parameters>,
    after_genesis: Vec<ScheduledEra<Parameters>>,
}

impl<Parameters> Eras<Parameters> {
    /// Resolves `entries`, listed in schedule order, for a chain that starts
    /// at `genesis_time`.
    pub fn new(
        genesis_time: OffsetDateTime,
        entries: impl IntoIterator<Item = EraEntry<Parameters>>,
    ) -> Result<Self, ErasError> {
        let mut entries = entries.into_iter();
        let genesis = entries.next().ok_or(ErasError::Empty)?;
        if genesis.first_epoch != Epoch::new(0) {
            return Err(ErasError::FirstEraAfterGenesis(genesis.first_epoch));
        }
        let genesis = ScheduledEra {
            era: Era::GENESIS,
            first_slot: Slot::genesis(),
            start_time: genesis_time,
            entry: genesis,
        };
        let mut after_genesis: Vec<ScheduledEra<Parameters>> = Vec::new();
        for entry in entries {
            let previous = after_genesis.last().unwrap_or(&genesis);
            let era = previous
                .era
                .into_inner()
                .checked_add(1)
                .map(Era::new)
                .ok_or(ErasError::TooManyEras)?;
            if entry.first_epoch <= previous.entry.first_epoch {
                return Err(ErasError::OutOfOrder {
                    previous: previous.entry.first_epoch,
                    next: entry.first_epoch,
                });
            }
            let (first_slot, start_time) = previous
                .boundary(entry.first_epoch)
                .ok_or(ErasError::Overflow(era))?;
            after_genesis.push(ScheduledEra {
                era,
                first_slot,
                start_time,
                entry,
            });
        }
        Ok(Self {
            genesis,
            after_genesis,
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
        usize::from(era.into_inner())
            .checked_sub(1)
            .map_or(Some(&self.genesis), |after_genesis| {
                self.after_genesis.get(after_genesis)
            })
    }

    /// Every era, in schedule order.
    pub fn iter(&self) -> impl Iterator<Item = &ScheduledEra<Parameters>> {
        core::iter::once(&self.genesis).chain(&self.after_genesis)
    }

    /// The same schedule, each era carrying what `f` makes of it instead of
    /// its parameters. Numbers, boundaries, versions and lengths are kept.
    pub fn map<Mapped>(
        &self,
        mut f: impl FnMut(&ScheduledEra<Parameters>) -> Mapped,
    ) -> Eras<Mapped> {
        let genesis = map_era(&self.genesis, f(&self.genesis));
        let after_genesis = self
            .after_genesis
            .iter()
            .map(|era| map_era(era, f(era)))
            .collect();
        Eras {
            genesis,
            after_genesis,
        }
    }

    /// The era `slot` belongs to.
    #[must_use]
    pub fn at_slot(&self, slot: Slot) -> &ScheduledEra<Parameters> {
        self.last_started(|era| era.first_slot <= slot)
    }

    /// The eras the network accepts at `slot`: the era of `slot`, and the era
    /// before it if `slot` is within the transition period of its era.
    #[must_use]
    pub fn in_force(&self, slot: Slot) -> EraInForce {
        let scheduled = self.at_slot(slot);
        let slots_into_era = slot
            .into_inner()
            .strict_sub(scheduled.first_slot.into_inner());
        let retiring = scheduled
            .era
            .into_inner()
            .checked_sub(1)
            .filter(|_| slots_into_era < scheduled.entry.transition_slots)
            .map(Era::new);
        EraInForce {
            era: scheduled.era,
            retiring,
        }
    }

    /// The era `epoch` belongs to.
    #[must_use]
    pub fn at_epoch(&self, epoch: Epoch) -> &ScheduledEra<Parameters> {
        self.last_started(|era| era.entry.first_epoch <= epoch)
    }

    /// The first slot of `epoch`, counted in the epoch length of its era.
    ///
    /// # Panics
    ///
    /// If the slot does not fit a [`Slot`].
    #[must_use]
    pub fn epoch_start(&self, epoch: Epoch) -> Slot {
        let era = self.at_epoch(epoch);
        let epochs_into_era = u64::from(
            epoch
                .into_inner()
                .strict_sub(era.entry.first_epoch.into_inner()),
        );
        Slot::new(
            era.first_slot
                .into_inner()
                .strict_add(epochs_into_era.strict_mul(era.entry.epoch_length.get())),
        )
    }

    /// The epoch `slot` belongs to, counted in the epoch length of its era.
    ///
    /// # Panics
    ///
    /// If the epoch does not fit an [`Epoch`].
    #[must_use]
    pub fn epoch_of(&self, slot: Slot) -> Epoch {
        let era = self.at_slot(slot);
        let epochs_into_era = slot.into_inner().strict_sub(era.first_slot.into_inner())
            / era.entry.epoch_length.get();
        let epoch = u64::from(era.entry.first_epoch.into_inner()).strict_add(epochs_into_era);
        Epoch::new(u32::try_from(epoch).expect("the epoch of a slot must fit an epoch number"))
    }

    /// The slot in progress at `time`, counted in the slot duration of its
    /// era: the genesis slot before genesis.
    ///
    /// # Panics
    ///
    /// If the slot does not fit a [`Slot`].
    #[must_use]
    pub fn slot_at(&self, time: OffsetDateTime) -> Slot {
        let era = self.last_started(|era| era.start_time <= time);
        // Negative only before genesis: every later era starts by `time`.
        let Ok(since_start) = u128::try_from((time - era.start_time).whole_nanoseconds()) else {
            return Slot::genesis();
        };
        let slots_into_era = u64::try_from(since_start / era.entry.slot_duration.as_nanos())
            .expect("the slot in progress must fit a slot number");
        Slot::new(era.first_slot.into_inner().strict_add(slots_into_era))
    }

    /// When `slot` starts, counted in the slot duration of its era.
    ///
    /// # Panics
    ///
    /// If the time does not fit an [`OffsetDateTime`].
    #[must_use]
    pub fn time_of(&self, slot: Slot) -> OffsetDateTime {
        let era = self.at_slot(slot);
        let slots_into_era = slot.into_inner().strict_sub(era.first_slot.into_inner());
        span(era.entry.slot_duration, slots_into_era)
            .and_then(|span| era.start_time.checked_add(span))
            .expect("the start of a slot must fit a date and time")
    }

    /// The last era `started` holds for, given that it holds for the eras up
    /// to some point of the schedule and for none after it: the genesis era
    /// when it holds for no later one.
    fn last_started(
        &self,
        started: impl FnMut(&ScheduledEra<Parameters>) -> bool,
    ) -> &ScheduledEra<Parameters> {
        match self.after_genesis.partition_point(started) {
            0 => &self.genesis,
            started_after_genesis => &self.after_genesis[started_after_genesis - 1],
        }
    }
}

const fn map_era<Parameters, Mapped>(
    era: &ScheduledEra<Parameters>,
    parameters: Mapped,
) -> ScheduledEra<Mapped> {
    ScheduledEra {
        era: era.era,
        first_slot: era.first_slot,
        start_time: era.start_time,
        entry: EraEntry {
            first_epoch: era.entry.first_epoch,
            version: era.entry.version,
            slot_duration: era.entry.slot_duration,
            epoch_length: era.entry.epoch_length,
            transition_slots: era.entry.transition_slots,
            parameters,
        },
    }
}

/// `slots` slots of `slot_duration` each, as a span of time. `None` on
/// overflow.
fn span(slot_duration: Duration, slots: u64) -> Option<time::Duration> {
    const NANOS_PER_SECOND: u128 = 1_000_000_000;
    let nanos = slot_duration.as_nanos().checked_mul(u128::from(slots))?;
    let seconds = i64::try_from(nanos / NANOS_PER_SECOND).ok()?;
    let subsecond_nanos = i32::try_from(nanos % NANOS_PER_SECOND).ok()?;
    Some(time::Duration::new(seconds, subsecond_nanos))
}

#[cfg(test)]
mod tests {
    use core::{num::NonZero, time::Duration};

    use time::OffsetDateTime;

    use super::{Era, EraEntry, EraInForce, EraVersion, Eras, ErasError};
    use crate::time::{Epoch, Slot};

    const GENESIS: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn entry(first_epoch: u32, slot_duration: Duration, epoch_length: u64) -> EraEntry<()> {
        EraEntry {
            first_epoch: Epoch::new(first_epoch),
            version: EraVersion::V1,
            slot_duration,
            epoch_length: NonZero::new(epoch_length).unwrap(),
            transition_slots: 10,
            parameters: (),
        }
    }

    /// Era 0: slots of 1 s, epochs of 100 slots. Era 1 from epoch 3: slots of
    /// 2 s, epochs of 50 slots. Era 2 from epoch 5: slots of 1.5 s, epochs of
    /// 3 slots. Era 3 from epoch 6: slots of 1 s, epochs of 100 slots.
    fn four_eras() -> Eras<()> {
        Eras::new(
            GENESIS,
            [
                entry(0, Duration::from_secs(1), 100),
                entry(3, Duration::from_secs(2), 50),
                entry(5, Duration::from_millis(1500), 3),
                entry(6, Duration::from_secs(1), 100),
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

        let slots = [
            -10.0, 0.0, 0.999, 1.0, 299.999, 300.0, 301.999, 302.0, 500.0, 501.499, 501.5, 504.5,
        ]
        .map(|at| (at, eras.slot_at(seconds(at)).into_inner()));
        assert_eq!(
            slots,
            [
                // Before genesis, the genesis slot is in progress.
                (-10.0, 0),
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
        let starts = [0, 1, 2, 3, 4, 5, 6, 7]
            .map(|epoch| (epoch, eras.epoch_start(Epoch::new(epoch)).into_inner()));
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
            let start = eras.epoch_start(epoch);
            assert_eq!(eras.epoch_of(start), epoch);
            assert_eq!(eras.at_epoch(epoch).era, eras.at_slot(start).era);
        }
    }

    #[test]
    fn every_slot_starts_when_the_previous_one_ends() {
        let eras = four_eras();
        for slot in (0..=600).map(Slot::new) {
            let start = eras.time_of(slot);
            assert_eq!(eras.slot_at(start), slot);
            let next_start = eras.time_of(Slot::new(slot.into_inner() + 1));
            assert_eq!(eras.slot_at(next_start - time::Duration::NANOSECOND), slot);
        }
    }

    #[test]
    fn the_era_before_is_accepted_during_the_transition_period() {
        let eras = four_eras();
        let in_force = [0, 299, 300, 309, 310, 400, 402, 403, 412, 413]
            .map(|slot| (slot, eras.in_force(Slot::new(slot))));
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
    fn invalid_schedules_are_rejected() {
        let second = Duration::from_secs(1);
        assert_eq!(Eras::<()>::new(GENESIS, []), Err(ErasError::Empty));
        assert_eq!(
            Eras::new(GENESIS, [entry(1, second, 100)]),
            Err(ErasError::FirstEraAfterGenesis(Epoch::new(1)))
        );
        assert_eq!(
            Eras::new(
                GENESIS,
                [
                    entry(0, second, 100),
                    entry(5, second, 100),
                    entry(5, second, 100)
                ]
            ),
            Err(ErasError::OutOfOrder {
                previous: Epoch::new(5),
                next: Epoch::new(5)
            })
        );
        // Two epochs of the genesis era already run past the last slot.
        assert_eq!(
            Eras::new(GENESIS, [entry(0, second, u64::MAX), entry(2, second, 100)]),
            Err(ErasError::Overflow(Era::new(1)))
        );
    }
}
