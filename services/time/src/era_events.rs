//! The era events the clock brings about: an era starting, and the transition
//! period it opens expiring.

use lb_time::{
    Era, Slot,
    era::{EraEntryView, EraSchedule},
};

use crate::{EraEvent, EraTransitionEnd, SlotTick, TimeServiceEraSettings};

/// The eras of the clock, followed from one slot tick to the next.
pub struct EraTransitionsTracker<'schedule> {
    schedule: &'schedule EraSchedule<TimeServiceEraSettings>,
    era_in_force: EraEntryView<'schedule, TimeServiceEraSettings>,
    /// The slot at which the transition period of the era before the one in
    /// force ends, while that period is open.
    previous_transition_ending_slot: Option<Slot>,
}

impl<'schedule> EraTransitionsTracker<'schedule> {
    pub fn new(
        current_slot_tick: SlotTick,
        schedule: &'schedule EraSchedule<TimeServiceEraSettings>,
    ) -> Self {
        Self {
            schedule,
            era_in_force: schedule
                .get(current_slot_tick.era)
                .expect("the clock is in a scheduled era"),
            previous_transition_ending_slot: None,
        }
    }

    pub const fn era_in_force(&self) -> &EraEntryView<'schedule, TimeServiceEraSettings> {
        &self.era_in_force
    }

    /// Follows the clock to `tick`, returning the era event it brings about, if
    /// any. A transition period that has already ended at the tick its era
    /// starts at is reported at the next tick.
    ///
    /// # Panics
    ///
    /// If an era starts before the transition period of the era before it has
    /// ended.
    #[must_use]
    pub fn advance(&mut self, tick: SlotTick) -> Option<EraEvent> {
        let current_era = self.era_in_force.era;
        let new_era = tick.era;
        if new_era > current_era {
            assert!(
                self.previous_transition_ending_slot.is_none(),
                "Cannot start a new era before the transition of the previous one has completed."
            );
            self.era_in_force = self
                .schedule
                .get(new_era)
                .expect("the clock only enters scheduled eras");
            self.previous_transition_ending_slot = Some(self.transition_ending_slot(current_era));
            return Some(EraEvent::NewEra {
                era: self.era_in_force.era,
                first_slot: self.era_in_force.first_slot,
            });
        }
        self.previous_transition_ending_slot
            .take_if(|ending_slot| tick.slot >= *ending_slot)
            .map(|_| {
                EraEvent::TransitionPeriodExpired(EraTransitionEnd {
                    era: Era::new(self.era_in_force.era.into_inner().strict_sub(1)),
                })
            })
    }

    /// The slot at which the transition period of `era` ends: the first slot of
    /// the next era that starts once `era`'s transition slots, at `era`'s slot
    /// duration, have elapsed since the next era started.
    fn transition_ending_slot(&self, era: Era) -> Slot {
        let era_settings = self
            .schedule
            .get(era)
            .expect("an era with a transition period is scheduled")
            .parameters;
        let next_era_schedule = self
            .schedule
            .get(Era::new(era.into_inner().strict_add(1)))
            .expect("a transition period opens when the next era starts");
        // In nanoseconds, so that slots of any length, sub-second ones included,
        // are measured exactly.
        let era_transition_period_in_nanos = era_settings
            .timing
            .slot_duration
            .as_nanos()
            .checked_mul(u128::from(era_settings.transition_slots))
            .expect("a transition period must fit in nanoseconds");
        let transition_period_duration_in_next_era_slots = era_transition_period_in_nanos
            .div_ceil(next_era_schedule.parameters.timing.slot_duration.as_nanos());
        Slot::new(
            next_era_schedule.first_slot.into_inner().strict_add(
                u64::try_from(transition_period_duration_in_next_era_slots)
                    .expect("a transition period must fit in slots"),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use core::{num::NonZero, time::Duration};

    use lb_time::{
        Epoch, Era, Slot,
        era::{EraEntriesAfterGenesis, EraSchedule, EraTiming},
    };
    use time::OffsetDateTime;

    use super::EraTransitionsTracker;
    use crate::{EraEvent, EraTransitionEnd, SlotTick, TimeServiceEraSettings};

    /// Era 0, with 1-second slots and a 30-slot transition period, and era 1
    /// from epoch 34, so from slot 102, with `era_1_slot_duration`.
    fn schedule(era_1_slot_duration: Duration) -> EraSchedule<TimeServiceEraSettings> {
        let timing = |slot_duration| EraTiming {
            slot_duration,
            epoch_length: NonZero::new(3).unwrap(),
        };
        EraSchedule::new(
            OffsetDateTime::UNIX_EPOCH,
            timing(Duration::from_secs(1)),
            EraEntriesAfterGenesis::from((NonZero::new(34).unwrap(), timing(era_1_slot_duration))),
        )
        .unwrap()
        .map(|era| TimeServiceEraSettings {
            timing: *era.parameters,
            transition_slots: 30,
        })
    }

    fn tick(era: u16, slot: u64) -> SlotTick {
        SlotTick {
            era: Era::new(era),
            // Not read: eras are followed by slot.
            epoch: Epoch::new(0),
            slot: Slot::new(slot),
        }
    }

    /// The era events `ticks` bring about, each with the slot of its tick.
    fn events_of(
        schedule: &EraSchedule<TimeServiceEraSettings>,
        start: SlotTick,
        ticks: impl IntoIterator<Item = SlotTick>,
    ) -> Vec<(u64, EraEvent)> {
        let mut transitions = EraTransitionsTracker::new(start, schedule);
        ticks
            .into_iter()
            .filter_map(|tick| {
                transitions
                    .advance(tick)
                    .map(|event| (tick.slot.into_inner(), event))
            })
            .collect()
    }

    const fn new_era(era: u16, slot: u64) -> EraEvent {
        EraEvent::NewEra {
            era: Era::new(era),
            first_slot: Slot::new(slot),
        }
    }

    const fn expired(era: u16) -> EraEvent {
        EraEvent::TransitionPeriodExpired(EraTransitionEnd { era: Era::new(era) })
    }

    #[test]
    fn the_previous_era_is_served_for_its_transition_period_after_a_new_era_starts() {
        let schedule = schedule(Duration::from_secs(1));
        let events = events_of(
            &schedule,
            tick(0, 100),
            (101..=133).map(|slot| tick(u16::from(slot >= 102), slot)),
        );
        // Era 1 starts at slot 102, and era 0's 30 slots end at slot 132.
        assert_eq!(events, [(102, new_era(1, 102)), (132, expired(0))]);
    }

    #[test]
    fn a_transition_period_ends_at_the_first_slot_that_starts_once_it_has_elapsed() {
        // Era 1's 4-second slots: when era 0's 30 one-second slots have elapsed
        // after slot 102 starts, slot 109 is in progress, so the period ends
        // when slot 110 starts.
        let schedule = schedule(Duration::from_secs(4));
        let events = events_of(
            &schedule,
            tick(0, 101),
            (102..=110).map(|slot| tick(1, slot)),
        );
        assert_eq!(events, [(102, new_era(1, 102)), (110, expired(0))]);
    }

    #[test]
    fn a_transition_period_is_measured_in_slots_of_any_length() {
        // Era 0's 30 one-second slots last 75 of era 1's slots of 400 ms, and
        // 20 of its slots of 1.5 s.
        for (era_1_slot_duration, ending_slot) in [
            (Duration::from_millis(400), 177),
            (Duration::from_millis(1500), 122),
        ] {
            let schedule = schedule(era_1_slot_duration);
            let events = events_of(
                &schedule,
                tick(0, 101),
                (102..=ending_slot).map(|slot| tick(1, slot)),
            );
            assert_eq!(events, [(102, new_era(1, 102)), (ending_slot, expired(0))]);
        }
    }

    #[test]
    fn a_clock_jumping_past_a_transition_period_reports_its_end_at_the_next_tick() {
        let schedule = schedule(Duration::from_secs(1));
        let events = events_of(&schedule, tick(0, 100), [tick(1, 140), tick(1, 141)]);
        assert_eq!(events, [(140, new_era(1, 102)), (141, expired(0))]);
    }

    #[test]
    fn a_clock_starting_within_a_transition_period_reports_nothing() {
        let schedule = schedule(Duration::from_secs(1));
        let events = events_of(
            &schedule,
            tick(1, 120),
            (121..=140).map(|slot| tick(1, slot)),
        );
        assert_eq!(events, []);
    }
}
