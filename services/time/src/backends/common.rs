use std::{pin::Pin, sync::Arc, time::Duration};

use futures::{Stream, StreamExt as _};
use lb_cryptarchia_engine::Slot;
use time::OffsetDateTime;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_stream::wrappers::IntervalStream;

use crate::{EpochSlotTickStream, EraSchedules, SlotTick};

/// Returns the current [`SlotTick`] and a stream of future [`SlotTick`]s
/// that ticks at the start of each slot, starting from the next slot.
pub fn slot_timer(
    eras: Arc<EraSchedules>,
    datetime: OffsetDateTime,
    current_slot: Slot,
) -> (SlotTick, EpochSlotTickStream) {
    (
        new_slot_tick(current_slot, &eras),
        Pin::new(Box::new(
            slot_interval(&eras, datetime)
                .zip(futures::stream::iter(std::iter::successors(
                    Some(current_slot.strict_add(1.into())), /* +1 because `slot_interval` ticks
                                                              * from the next slot */
                    |&slot| Some(slot.strict_add(1.into())),
                )))
                .map(move |(_, slot)| new_slot_tick(slot, &eras)),
        )),
    )
}

fn slot_interval(
    eras: &EraSchedules,
    start: OffsetDateTime,
) -> impl Stream<Item = Instant> + use<> {
    let now = Instant::now();
    let next_slot = eras
        .slot_at(start)
        .unwrap_or(Slot::genesis())
        .strict_add(1.into());
    let next_slot_era = eras.at_slot(next_slot).era;

    let current_and_future_eras_reversed = eras
        .iter()
        .rev()
        .take_while(|scheduled_era| scheduled_era.era >= next_slot_era);
    let current_and_future_eras_ending_slots = current_and_future_eras_reversed
        .scan(None, |next_era_starting_slot, era| {
            Some((era, next_era_starting_slot.replace(era.first_slot)))
        });
    let current_and_future_eras_intervals = {
        let mut current_and_future_eras_intervals_reversed = current_and_future_eras_ending_slots
            .map(|(era, era_end)| {
                // This only applies to the current era, so we don't start from its first slot
                // but from the next slot.
                let era_first_ticking_slot = next_slot.max(era.first_slot);
                // `era_end` is `None` for the last scheduled era. In that case `u64::MAX`
                // means we will never stop that era tick.
                let era_slots_count = era_end.map_or(u64::MAX, |end| {
                    end.into_inner()
                        .strict_sub(era_first_ticking_slot.into_inner())
                });
                let tick_delay_from_start = eras.time_of(era_first_ticking_slot) - start;
                let interval = {
                    let mut interval = interval_at(
                        now + Duration::try_from(tick_delay_from_start)
                            .expect("could not set slot timer duration"),
                        era.entry.slot_duration,
                    );
                    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                    interval
                };
                IntervalStream::new(interval).take(era_slots_count.try_into().unwrap())
            })
            .collect::<Vec<_>>();
        // Back in schedule order.
        current_and_future_eras_intervals_reversed.reverse();
        current_and_future_eras_intervals_reversed
    };
    futures::stream::iter(current_and_future_eras_intervals).flatten()
}

fn new_slot_tick(slot: Slot, eras: &EraSchedules) -> SlotTick {
    SlotTick {
        era: eras.at_slot(slot).era,
        epoch: eras.epoch_of(slot),
        slot,
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use lb_cryptarchia_engine::{
        Epoch,
        era::{Era, EraEntriesAfterGenesis, EraEntry, EraSchedule, EraVersion},
    };

    use super::*;

    #[tokio::test]
    async fn test_slot_timer() {
        let (current_slot_tick, mut timer, eras) = timer();

        // Calculate the expected slot based on the current time.
        let mut expected_slot = eras.slot_at(OffsetDateTime::now_utc()).unwrap();
        assert_eq!(current_slot_tick.slot, expected_slot);

        // The first tick will be the next slot after the timer was created.
        let tick = timer.next().await;
        // Slots should increment by 1 for each tick.
        expected_slot = expected_slot.strict_add(1.into());
        assert_eq!(tick.unwrap().slot, expected_slot);

        // Slots should increment by 1 for each tick.
        let tick = timer.next().await;
        expected_slot = expected_slot.strict_add(1.into());
        assert_eq!(tick.unwrap().slot, expected_slot);
    }

    #[tokio::test(start_paused = true)]
    async fn ticks_follow_the_slot_duration_of_each_era() {
        // Slots of 1 s in epochs of 2 slots, then from epoch 2 (slot 4) slots of
        // 3 s in epochs of 2 slots.
        let genesis = OffsetDateTime::UNIX_EPOCH;
        let eras = Arc::new(
            EraSchedule::new(
                genesis,
                entry(Duration::from_secs(1), 2),
                EraEntriesAfterGenesis::from((
                    NonZero::new(2).unwrap(),
                    entry(Duration::from_secs(3), 2),
                )),
            )
            .unwrap(),
        );
        let now = genesis + Duration::from_millis(2500);
        let (current, mut timer) = slot_timer(Arc::clone(&eras), now, eras.slot_at(now).unwrap());
        assert_eq!(current, tick(0, 1, 2));

        // Slot 3 starts after 0.5 s, and slot 4, the first of era 1, 1 s later.
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(timer.next().await, Some(tick(0, 1, 3)));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(timer.next().await, Some(tick(1, 2, 4)));

        // Slots of era 1 last 3 s.
        tokio::time::advance(Duration::from_millis(2999)).await;
        assert!(futures::poll!(timer.next()).is_pending());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(timer.next().await, Some(tick(1, 2, 5)));
        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(timer.next().await, Some(tick(1, 3, 6)));
    }

    fn timer() -> (SlotTick, EpochSlotTickStream, Arc<EraSchedules>) {
        let now = OffsetDateTime::now_utc();
        let eras = Arc::new(
            EraSchedule::new(
                now,
                entry(Duration::from_secs(1), 3),
                EraEntriesAfterGenesis::empty(),
            )
            .unwrap(),
        );
        let (current_slot_tick, timer) = slot_timer(Arc::clone(&eras), now, Slot::from(0));
        (current_slot_tick, timer, eras)
    }

    fn entry(slot_duration: Duration, epoch_length: u64) -> EraEntry<()> {
        EraEntry {
            version: EraVersion::V1,
            slot_duration,
            epoch_length_in_slots: NonZero::new(epoch_length).unwrap(),
            transition_slots: 0,
            parameters: (),
        }
    }

    fn tick(era: u16, epoch: u32, slot: u64) -> SlotTick {
        SlotTick {
            era: Era::new(era),
            epoch: Epoch::new(epoch),
            slot: Slot::new(slot),
        }
    }
}
