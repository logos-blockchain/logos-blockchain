use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
    time::Duration,
};

use futures::Stream;
use lb_cryptarchia_engine::{Slot, era::Eras};
use time::OffsetDateTime;
use tokio::time::{Instant, Sleep, sleep_until};

use crate::{EpochSlotTickStream, SlotTick};

/// Returns the [`SlotTick`] in progress at `now` and a stream of the next ones,
/// ticking at the start of each slot from the next one on, as `eras` lays the
/// slots out in time.
pub fn slot_timer(eras: Arc<Eras<()>>, now: OffsetDateTime) -> (SlotTick, EpochSlotTickStream) {
    let current_slot = eras.slot_at(now);
    let current_tick = slot_tick(&eras, current_slot);
    (
        current_tick,
        Box::pin(SlotTimer::new(eras, now, current_slot)),
    )
}

/// The tick of `slot`, with its epoch and era.
pub fn slot_tick<Parameters>(eras: &Eras<Parameters>, slot: Slot) -> SlotTick {
    SlotTick {
        era: eras.at_slot(slot).era,
        epoch: eras.epoch_of(slot),
        slot,
    }
}

/// Ticks at the start of each slot after the one it was created in. Every slot
/// lasts the slot duration of its own era, so a boundary between eras with
/// different slot durations needs no special handling.
struct SlotTimer {
    eras: Arc<Eras<()>>,
    /// The wall-clock time `started` stands for. Deadlines are tokio instants,
    /// derived from wall-clock times through this pair, so that tests can
    /// control them.
    now: OffsetDateTime,
    started: Instant,
    last_slot: Slot,
    sleep: Pin<Box<Sleep>>,
}

impl SlotTimer {
    fn new(eras: Arc<Eras<()>>, now: OffsetDateTime, current_slot: Slot) -> Self {
        let started = Instant::now();
        let mut timer = Self {
            eras,
            now,
            started,
            last_slot: current_slot,
            sleep: Box::pin(sleep_until(started)),
        };
        let first_deadline = timer.deadline(current_slot.strict_add(1.into()));
        timer.sleep.as_mut().reset(first_deadline);
        timer
    }

    /// The instant `slot` starts at. A slot that started before the timer did
    /// is due right away.
    fn deadline(&self, slot: Slot) -> Instant {
        let until = Duration::try_from(self.eras.time_of(slot) - self.now).unwrap_or_default();
        self.started + until
    }
}

impl Stream for SlotTimer {
    type Item = SlotTick;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        ready!(this.sleep.as_mut().poll(cx));
        // A late wake-up, the task having been held up, ticks for the slot in
        // progress: the slots it missed are skipped.
        let in_progress = this.eras.slot_at(this.now + this.started.elapsed());
        let slot = in_progress.max(this.last_slot.strict_add(1.into()));
        this.last_slot = slot;
        let next_deadline = this.deadline(slot.strict_add(1.into()));
        this.sleep.as_mut().reset(next_deadline);
        Poll::Ready(Some(slot_tick(&this.eras, slot)))
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZero;

    use futures::StreamExt as _;
    use lb_cryptarchia_engine::{
        Epoch,
        era::{Era, EraEntry, EraVersion},
    };

    use super::*;

    fn entry(first_epoch: u32, slot_duration: Duration, epoch_length: u64) -> EraEntry<()> {
        EraEntry {
            first_epoch: Epoch::new(first_epoch),
            version: EraVersion::V1,
            slot_duration,
            epoch_length: NonZero::new(epoch_length).unwrap(),
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

    #[tokio::test(start_paused = true)]
    async fn ticks_follow_the_slot_duration_of_each_era() {
        // Slots of 1 s in epochs of 2 slots, then from epoch 2 (slot 4) slots of
        // 3 s in epochs of 2 slots.
        let genesis = OffsetDateTime::UNIX_EPOCH;
        let eras = Arc::new(
            Eras::new(
                genesis,
                [
                    entry(0, Duration::from_secs(1), 2),
                    entry(2, Duration::from_secs(3), 2),
                ],
            )
            .unwrap(),
        );
        let (current, mut timer) = slot_timer(eras, genesis + Duration::from_millis(2500));
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

    #[tokio::test(start_paused = true)]
    async fn a_late_tick_skips_the_slots_it_missed() {
        let genesis = OffsetDateTime::UNIX_EPOCH;
        let eras = Arc::new(Eras::new(genesis, [entry(0, Duration::from_secs(1), 10)]).unwrap());
        let (current, mut timer) = slot_timer(eras, genesis);
        assert_eq!(current, tick(0, 0, 0));

        tokio::time::advance(Duration::from_millis(3500)).await;
        assert_eq!(timer.next().await, Some(tick(0, 0, 3)));
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(timer.next().await, Some(tick(0, 0, 4)));
    }
}
