//! The eras of a chain, resolved into slots and wall-clock time.
//!
//! An era is a range of consecutive epochs governed by one set of parameters.
//! Eras are numbered from 0, the era that starts at genesis. Every era has its
//! own slot duration and epoch length, so its boundaries are resolved era by
//! era: an era's first slot follows the previous era's last epoch, measured in
//! the previous era's epoch length, and its start time follows the previous
//! era's last slot, measured in the previous era's slot duration.

use core::{iter::once, num::NonZero, time::Duration};
use std::collections::BTreeMap;

use lb_utils::{
    bounded::UpperBoundedBTreeMap,
    bounded_duration::{MinimalBoundedDuration, SECOND},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::time::{Epoch, Slot};

/// An era, by its number: its position in its chain's era schedule, counting
/// from 0 for the era that starts at genesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EraNumber(u16);

impl EraNumber {
    #[must_use]
    pub const fn new(inner: u16) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> u16 {
        self.0
    }

    #[must_use]
    pub const fn genesis() -> Self {
        Self(0)
    }
}

/// The eras of a chain after its genesis era, as its schedule lists them, each
/// keyed by the epoch it starts at. The genesis era starts at epoch 0, so no
/// era after it can.
pub type EraEntriesAfterGenesis<Parameters> =
    UpperBoundedBTreeMap<NonZero<u32>, EraEntry<Parameters>, { u16::MAX as usize }>;

/// An era as a schedule lists it: the length of its slots and epochs, and what
/// it carries.
///
/// Anything that is not common to all era definitions is included in the
/// `Parameters` type.
#[serde_with::serde_as]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EraEntry<Parameters> {
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
    pub epoch_length_in_slots: NonZero<u64>,
    pub parameters: Parameters,
}

/// An era of a schedule, resolved: the era as the schedule lists it, its
/// number, the epoch it starts at, and where it starts in slots and in time,
/// which follow from every era before it.
#[derive(Debug, PartialEq, Eq)]
// non_exhaustive used to allow consumers to access the struct fields and match them without
// allowing them to create one directly.
#[non_exhaustive]
pub struct EraEntryView<'schedule, Parameters> {
    pub number: EraNumber,
    pub first_epoch: Epoch,
    pub first_slot: Slot,
    pub start_time: OffsetDateTime,
    pub entry: &'schedule EraEntry<Parameters>,
}

impl<Parameters> Clone for EraEntryView<'_, Parameters> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Parameters> Copy for EraEntryView<'_, Parameters> {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredEntry<Parameters> {
    first_slot: Slot,
    start_time: OffsetDateTime,
    entry: EraEntry<Parameters>,
}

impl<Parameters> StoredEntry<Parameters> {
    const fn view(&self, number: EraNumber, first_epoch: Epoch) -> EraEntryView<'_, Parameters> {
        EraEntryView {
            number,
            first_epoch,
            first_slot: self.first_slot,
            start_time: self.start_time,
            entry: &self.entry,
        }
    }
}

/// Why a schedule's eras cannot be resolved.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ErasError {
    #[error("era {} starts beyond the slots or the time this node can represent", .0.into_inner())]
    Overflow(EraNumber),
}

/// A chain's eras, each resolved against the ones before it.
///
/// Never empty, and the first era starts at genesis: at epoch 0, slot 0 and
/// the genesis time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EraSchedule<Parameters> {
    genesis: StoredEntry<Parameters>,
    after_genesis: BTreeMap<NonZero<u32>, StoredEntry<Parameters>>,
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
        /// Where the era after `era` starts, in slots and in time, if it
        /// starts at `next_era_first_epoch`: measured in `era`'s epoch length
        /// and slot duration, since every slot before it is `era`'s. `None` on
        /// overflow.
        fn next_era_start<Parameters>(
            era: EraEntryView<'_, Parameters>,
            next_era_first_epoch: Epoch,
        ) -> Option<(Slot, OffsetDateTime)> {
            let epochs_in_era = u64::from(
                next_era_first_epoch
                    .into_inner()
                    .checked_sub(era.first_epoch.into_inner())?,
            );
            let slots_in_era = epochs_in_era.checked_mul(era.entry.epoch_length_in_slots.get())?;
            let next_era_first_slot =
                Slot::new(era.first_slot.into_inner().checked_add(slots_in_era)?);
            let next_era_start_time = era
                .start_time
                .checked_add(span(era.entry.slot_duration, slots_in_era)?)?;
            Some((next_era_first_slot, next_era_start_time))
        }

        let mut schedule = Self {
            genesis: StoredEntry {
                first_slot: Slot::genesis(),
                start_time: genesis_time,
                entry: genesis,
            },
            after_genesis: BTreeMap::new(),
        };
        for (first_epoch, entry) in after_genesis {
            let previous_era = schedule
                .iter()
                // Always refers to the last element in the schedule, so the newly added one on each
                // iteration, or the genesis on the first iteration.
                .next_back()
                .expect("a schedule has at least its genesis era");
            let era_number =
                EraNumber::new(previous_era.number.into_inner().checked_add(1).unwrap());
            let (first_slot, start_time) = next_era_start(previous_era, epoch_of_key(first_epoch))
                .ok_or(ErasError::Overflow(era_number))?;
            schedule.after_genesis.insert(
                first_epoch,
                StoredEntry {
                    first_slot,
                    start_time,
                    entry,
                },
            );
        }
        Ok(schedule)
    }

    /// Era 0, the era that starts at genesis.
    #[must_use]
    pub const fn genesis(&self) -> EraEntryView<'_, Parameters> {
        self.genesis.view(EraNumber::genesis(), Epoch::genesis())
    }

    /// Era `era`, if the schedule has it.
    #[must_use]
    pub fn get(&self, era: EraNumber) -> Option<EraEntryView<'_, Parameters>> {
        self.iter().nth(usize::from(era.into_inner()))
    }

    /// Every era, in schedule order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = EraEntryView<'_, Parameters>> {
        once(self.genesis()).chain(self.eras_after_genesis().map(|(_, era)| era))
    }

    /// The same schedule, each era carrying what `f` makes of it instead of
    /// its parameters. Numbers, boundaries and lengths are kept.
    pub fn map<MapFn, Mapped>(&self, mut map_fn: MapFn) -> EraSchedule<Mapped>
    where
        MapFn: FnMut(EraEntryView<'_, Parameters>) -> Mapped,
    {
        let genesis = map_era_values(self.genesis(), map_fn(self.genesis()));
        let after_genesis = self
            .eras_after_genesis()
            .map(|(first_epoch, era)| (first_epoch, map_era_values(era, map_fn(era))))
            .collect();
        EraSchedule {
            genesis,
            after_genesis,
        }
    }

    /// The era `slot` belongs to.
    #[must_use]
    pub fn at_slot(&self, slot: Slot) -> EraEntryView<'_, Parameters> {
        self.last_started(|era| era.first_slot <= slot)
            .expect("At least genesis era fulfils this predicate.")
    }

    pub fn elapsed_slots_since_era_start(&self, slot: Slot) -> u64 {
        let era_scheduled = self.at_slot(slot);
        slot.into_inner()
            .strict_sub(era_scheduled.first_slot.into_inner())
    }

    /// The era `epoch` belongs to.
    #[must_use]
    pub fn at_epoch(&self, epoch: Epoch) -> EraEntryView<'_, Parameters> {
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
    pub fn starting_slot_for_epoch(&self, epoch: Epoch) -> Slot {
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
    pub fn epoch_for_slot(&self, slot: Slot) -> Epoch {
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

    /// The eras after genesis, in schedule order, each with its key.
    fn eras_after_genesis(
        &self,
    ) -> impl DoubleEndedIterator<Item = (NonZero<u32>, EraEntryView<'_, Parameters>)> {
        self.after_genesis
            .iter()
            .enumerate()
            .map(|(index, (&first_epoch, era_view))| {
                let number = u16::try_from(index + 1).unwrap();
                (
                    first_epoch,
                    era_view.view(EraNumber::new(number), epoch_of_key(first_epoch)),
                )
            })
    }

    /// The last era `condition` holds for, given that it holds for the eras up
    /// to some point of the schedule and for none after it.
    fn last_started<Condition>(&self, condition: Condition) -> Option<EraEntryView<'_, Parameters>>
    where
        Condition: FnMut(&EraEntryView<'_, Parameters>) -> bool,
    {
        self.iter().take_while(condition).last()
    }
}

/// The epoch a key of the eras after genesis names.
const fn epoch_of_key(first_epoch: NonZero<u32>) -> Epoch {
    Epoch::new(first_epoch.get())
}

/// `slots` slots of `slot_duration` each, as a span of time. `None` on
/// overflow.
fn span(slot_duration: Duration, slots: u64) -> Option<time::Duration> {
    let nanos = slot_duration.as_nanos().checked_mul(u128::from(slots))?;
    // `from_nanos_u128` panics past `Duration::MAX`.
    let span = (nanos <= Duration::MAX.as_nanos()).then(|| Duration::from_nanos_u128(nanos))?;
    time::Duration::try_from(span).ok()
}

const fn map_era_values<Parameters, Mapped>(
    EraEntryView {
        entry:
            EraEntry {
                epoch_length_in_slots,
                slot_duration,
                ..
            },
        first_slot,
        start_time,
        ..
    }: EraEntryView<'_, Parameters>,
    parameters: Mapped,
) -> StoredEntry<Mapped> {
    StoredEntry {
        first_slot,
        start_time,
        entry: EraEntry {
            slot_duration: *slot_duration,
            epoch_length_in_slots: *epoch_length_in_slots,
            parameters,
        },
    }
}

#[cfg(test)]
mod tests {
    use core::{num::NonZero, time::Duration};

    use time::OffsetDateTime;

    use super::{EraEntriesAfterGenesis, EraEntry, EraNumber, EraSchedule, ErasError};
    use crate::time::{Epoch, Slot};

    const GENESIS: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn entry(slot_duration: Duration, epoch_length: u64) -> EraEntry<()> {
        EraEntry {
            slot_duration,
            epoch_length_in_slots: NonZero::new(epoch_length).unwrap(),
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
                (slot, era.number, era.first_slot, era.start_time)
            })
            .into_iter()
            .collect();
        assert_eq!(
            boundaries,
            [
                (0, EraNumber::genesis(), Slot::genesis(), GENESIS),
                (299, EraNumber::genesis(), Slot::genesis(), GENESIS),
                // 3 epochs of 100 slots of 1 s.
                (300, EraNumber::new(1), Slot::new(300), seconds(300.0)),
                (399, EraNumber::new(1), Slot::new(300), seconds(300.0)),
                // Then 2 epochs of 50 slots of 2 s.
                (400, EraNumber::new(2), Slot::new(400), seconds(500.0)),
                (402, EraNumber::new(2), Slot::new(400), seconds(500.0)),
                // Then 1 epoch of 3 slots of 1.5 s.
                (403, EraNumber::new(3), Slot::new(403), seconds(504.5)),
            ]
        );
    }

    #[test]
    fn slots_and_epochs_are_counted_in_the_units_of_their_era() {
        let eras = four_eras();
        let epochs = [0, 99, 100, 299, 300, 349, 350, 399, 400, 402, 403, 502, 503]
            .map(|slot| (slot, eras.epoch_for_slot(Slot::new(slot)).into_inner()));
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
                eras.starting_slot_for_epoch(Epoch::new(epoch)).into_inner(),
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
            let start = eras.starting_slot_for_epoch(epoch);
            assert_eq!(eras.epoch_for_slot(start), epoch);
            assert_eq!(eras.at_epoch(epoch).number, eras.at_slot(start).number);
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
    fn eras_are_found_by_number() {
        let eras = four_eras();
        for era in eras.iter() {
            assert_eq!(eras.get(era.number), Some(era));
        }
        assert_eq!(eras.get(EraNumber::new(4)), None);
    }

    #[test]
    fn a_mapped_schedule_keeps_its_boundaries() {
        let eras = four_eras();
        let numbers = eras.map(|era| era.number);
        for slot in [0, 300, 400, 403].map(Slot::new) {
            let era = numbers.at_slot(slot);
            assert_eq!(era.entry.parameters, era.number);
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
            Err(ErasError::Overflow(EraNumber::new(1)))
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
            Err(ErasError::Overflow(EraNumber::new(1)))
        );
    }
}
