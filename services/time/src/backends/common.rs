use std::{future::ready, pin::Pin, sync::Arc, time::Duration};

use futures::{Stream, StreamExt as _};
use lb_time::{Slot, era::EraTimingSchedule};
use time::OffsetDateTime;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_stream::wrappers::IntervalStream;

use crate::{EpochSlotTickStream, SlotTick};

/// Returns the current [`SlotTick`] and a stream of future [`SlotTick`]s
/// that ticks at the start of each slot, starting from the next slot.
///
/// A stream that is not polled in time delivers the first slot it missed late
/// and skips the others, so the slots after them, and the eras they start,
/// still tick on time.
pub fn slot_timer(
    era_schedules: Arc<EraTimingSchedule>,
    datetime: OffsetDateTime,
    current_slot: Slot,
) -> (SlotTick, EpochSlotTickStream) {
    (
        new_slot_tick(current_slot, &era_schedules),
        Pin::new(Box::new(
            slot_interval(&era_schedules, datetime)
                .map(move |slot| new_slot_tick(slot, &era_schedules)),
        )),
    )
}

fn new_slot_tick(slot: Slot, era_schedules: &EraTimingSchedule) -> SlotTick {
    SlotTick {
        era: era_schedules.at_slot(slot).era,
        epoch: era_schedules.epoch_for_slot(slot),
        slot,
    }
}

/// The slots from the one after `start_time` on, each when it starts.
fn slot_interval(
    era_schedules: &EraTimingSchedule,
    start_time: OffsetDateTime,
) -> impl Stream<Item = Slot> + use<> {
    let now = Instant::now();
    let next_slot = era_schedules
        .slot_at(start_time)
        .unwrap_or(Slot::genesis())
        .strict_add(1.into());
    let next_slot_era = era_schedules.at_slot(next_slot).era;

    // Take the era schedule, reverse it, and keep track of each era's starting slot
    // to be used as the previous era ending slot. The first era in the reversed
    // order (i.e., the last era of the schedule) will yield a forever interval
    // since it does not have a scheduled end.
    let current_and_future_eras_reversed = era_schedules
        .iter()
        .rev()
        .take_while(|era| era.era >= next_slot_era);
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
                let tick_delay_from_start =
                    era_schedules.time_of(era_first_ticking_slot) - start_time;
                let first_tick = now
                    + Duration::try_from(tick_delay_from_start)
                        .expect("could not set slot timer duration");
                let slot_duration = era.parameters.slot_duration;
                let mut interval = interval_at(first_tick, slot_duration);
                interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                // A tick missed while the stream is not polled is skipped, so the slot
                // of a tick follows from the time it was scheduled for, not from the
                // number of ticks before it. The era ends with the first tick at or
                // past the first slot of the next era; `era_end` is `None` for the
                // last scheduled era, which never ends.
                IntervalStream::new(interval)
                    .map(move |tick| {
                        let slots_since_first_tick =
                            (tick - first_tick).as_nanos() / slot_duration.as_nanos();
                        era_first_ticking_slot.strict_add(Slot::new(
                            slots_since_first_tick
                                .try_into()
                                .expect("the slots of an era fit in a `u64`"),
                        ))
                    })
                    .take_while(move |slot| ready(era_end.is_none_or(|end| *slot < end)))
            })
            .collect::<Vec<_>>();
        // Reverse again, to yield intervals back in the era schedule order.
        current_and_future_eras_intervals_reversed.reverse();
        current_and_future_eras_intervals_reversed
    };
    futures::stream::iter(current_and_future_eras_intervals).flatten()
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use lb_time::{
        Epoch, Era,
        era::{EraEntriesAfterGenesis, EraTiming},
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
            EraTimingSchedule::new(
                genesis,
                timing(Duration::from_secs(1), 2),
                EraEntriesAfterGenesis::from((
                    NonZero::new(2).unwrap(),
                    timing(Duration::from_secs(3), 2),
                )),
            )
            .unwrap(),
        );
        let now = genesis + Duration::from_millis(2500);
        let (current, mut timer) = slot_timer(Arc::clone(&eras), now, eras.slot_at(now).unwrap());
        assert_eq!(current, tick(0, 1, 2));

        // Slot 3 starts after 0.5 s, and slot 4, the first of the second era,
        // 1 s later.
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(timer.next().await, Some(tick(0, 1, 3)));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(timer.next().await, Some(tick(1, 2, 4)));

        // Slots of the second era last 3 s.
        tokio::time::advance(Duration::from_millis(2999)).await;
        assert!(futures::poll!(timer.next()).is_pending());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(timer.next().await, Some(tick(1, 2, 5)));
        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(timer.next().await, Some(tick(1, 3, 6)));
    }

    #[tokio::test(start_paused = true)]
    async fn missed_ticks_hold_back_neither_the_slots_nor_the_next_era() {
        // Slots of 1 s in epochs of 2 slots, then from epoch 2 (slot 4) slots of
        // 3 s in epochs of 2 slots.
        let genesis = OffsetDateTime::UNIX_EPOCH;
        let eras = Arc::new(
            EraTimingSchedule::new(
                genesis,
                timing(Duration::from_secs(1), 2),
                EraEntriesAfterGenesis::from((
                    NonZero::new(2).unwrap(),
                    timing(Duration::from_secs(3), 2),
                )),
            )
            .unwrap(),
        );
        let now = genesis + Duration::from_millis(500);
        let (current, mut timer) = slot_timer(Arc::clone(&eras), now, eras.slot_at(now).unwrap());
        assert_eq!(current, tick(0, 0, 0));

        // Nothing polls the stream through slots 1 to 3: slot 1 is delivered
        // late, and slots 2 and 3 are skipped.
        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(timer.next().await, Some(tick(0, 0, 1)));

        // The next era still starts on time, at slot 4, and its slots last 3 s.
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(timer.next().await, Some(tick(1, 2, 4)));
        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(timer.next().await, Some(tick(1, 2, 5)));
    }

    fn timer() -> (SlotTick, EpochSlotTickStream, Arc<EraTimingSchedule>) {
        let now = OffsetDateTime::now_utc();
        let eras = Arc::new(
            EraTimingSchedule::new(
                now,
                timing(Duration::from_secs(1), 3),
                EraEntriesAfterGenesis::empty(),
            )
            .unwrap(),
        );
        let (current_slot_tick, timer) = slot_timer(Arc::clone(&eras), now, Slot::from(0));
        (current_slot_tick, timer, eras)
    }

    fn timing(slot_duration: Duration, epoch_length: u64) -> EraTiming {
        EraTiming {
            slot_duration,
            epoch_length: NonZero::new(epoch_length).unwrap(),
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
