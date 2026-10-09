use std::time::{Duration, Instant, SystemTime};

use lb_common_http_client::Slot;
use lb_time::era::EraSchedules;
use time::OffsetDateTime;

/// Slack after a slot boundary so a wake-up lands inside the new slot; tokio
/// timers tick at millisecond granularity.
const BOUNDARY_GRACE: Duration = Duration::from_millis(10);

#[derive(Clone, Debug)]
pub(super) struct SlotClock {
    /// The chain's eras, which lay its slots out in time, each era in its own
    /// slot duration.
    eras: EraSchedules,
    last_observed_slot: Slot,
    last_observed_at: Instant,
}

impl SlotClock {
    pub(super) fn from_era_schedule(eras: EraSchedules) -> Self {
        let current_slot = slot_at(&eras, OffsetDateTime::now_utc());

        Self {
            eras,
            last_observed_slot: current_slot,
            last_observed_at: Instant::now(),
        }
    }

    pub(super) fn observe_slot(&mut self, observed_slot: Slot) {
        self.last_observed_slot = observed_slot;
        self.last_observed_at = Instant::now();
    }

    pub(super) fn current_slot(&self) -> Slot {
        let from_chain_start = slot_at(&self.eras, OffsetDateTime::now_utc());
        // The observed slot is taken to have started when it was observed.
        let from_anchor = self
            .eras
            .checked_time_of(self.last_observed_slot)
            .and_then(|observed| {
                observed.checked_add(time::Duration::try_from(self.last_observed_at.elapsed()).ok()?)
            })
            .map_or(self.last_observed_slot, |now| slot_at(&self.eras, now));

        from_chain_start.max(from_anchor)
    }

    /// Sleep until [`Self::current_slot`] has reached `slot`; `None` if the
    /// slot lies beyond what the clock can hold.
    pub(super) fn sleep_until(&self, slot: Slot) -> Option<tokio::time::Sleep> {
        let at = if self.current_slot() >= slot {
            Instant::now()
        } else {
            self.instant_of(slot)? + BOUNDARY_GRACE
        };
        Some(tokio::time::sleep_until(tokio::time::Instant::from_std(at)))
    }

    /// Earliest instant at which [`Self::current_slot`] reaches `slot`; now
    /// if it already has, `None` if it lies beyond what the clock can hold.
    fn instant_of(&self, slot: Slot) -> Option<Instant> {
        let now = Instant::now();
        let start = self.eras.checked_time_of(slot)?;

        let from_anchor = self
            .eras
            .checked_time_of(self.last_observed_slot)
            .and_then(|observed| {
                // A slot before the observed one is reached at the anchor.
                let since_observed = Duration::try_from(start - observed).unwrap_or(Duration::ZERO);
                self.last_observed_at.checked_add(since_observed)
            });

        let from_chain_start = Some(system_time_to_instant(SystemTime::from(start), now));

        // `current_slot` takes the later of its two slot estimates, so the
        // slot is reached at the earlier of the two instants.
        from_anchor
            .into_iter()
            .chain(from_chain_start)
            .min()
            .map(|at| at.max(now))
    }
}

/// The slot in progress at `time`: the genesis slot before genesis.
fn slot_at(eras: &EraSchedules, time: OffsetDateTime) -> Slot {
    eras.slot_at(time).unwrap_or(Slot::genesis())
}

fn system_time_to_instant(at: SystemTime, now: Instant) -> Instant {
    at.duration_since(SystemTime::now())
        .map_or(now, |until| now + until)
}

pub(super) const fn slot_to_u64(slot: Slot) -> u64 {
    slot.into_inner()
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use lb_time::era::{EraEntriesAfterGenesis, EraEntry, EraSchedule};

    use super::*;

    /// Eras starting at genesis `genesis`, slots of `slot_duration` in epochs
    /// of 2 slots, then from `next` the slot duration paired with it.
    fn eras(
        genesis: OffsetDateTime,
        slot_duration: Duration,
        next: Option<(u32, Duration)>,
    ) -> EraSchedules {
        let entry = |slot_duration| EraEntry {
            slot_duration,
            epoch_length_in_slots: NonZero::new(2).unwrap(),
            parameters: (),
        };
        let after_genesis = next.map_or_else(EraEntriesAfterGenesis::empty, |(epoch, duration)| {
            EraEntriesAfterGenesis::from((NonZero::new(epoch).unwrap(), entry(duration)))
        });
        EraSchedule::new(genesis, entry(slot_duration), after_genesis).unwrap()
    }

    #[test]
    fn instant_of_uses_the_earlier_of_anchor_and_chain_start() {
        let slot_duration = Duration::from_millis(100);
        let mut clock = SlotClock::from_era_schedule(eras(
            OffsetDateTime::now_utc(),
            slot_duration,
            None,
        ));
        clock.observe_slot(Slot::from(10));
        let anchor = clock.last_observed_at;

        let at = clock.instant_of(Slot::from(13)).unwrap();
        assert!(at >= anchor + slot_duration * 3);
        assert!(at < anchor + slot_duration * 4);

        assert!(clock.instant_of(Slot::from(5)).unwrap() <= Instant::now());
    }

    #[test]
    fn instant_of_follows_the_slot_duration_of_each_era() {
        // Slots of 100 ms until epoch 3, that is slot 6, then slots of 300 ms.
        let mut clock = SlotClock::from_era_schedule(eras(
            OffsetDateTime::now_utc(),
            Duration::from_millis(100),
            Some((3, Duration::from_millis(300))),
        ));
        clock.observe_slot(Slot::from(5));
        let anchor = clock.last_observed_at;

        // One slot of 100 ms, then two of 300 ms.
        let at = clock.instant_of(Slot::from(8)).unwrap();
        assert!(at >= anchor + Duration::from_millis(700));
        assert!(at < anchor + Duration::from_millis(800));
    }

    #[tokio::test]
    async fn sleep_until_a_reached_slot_does_not_wait() {
        let slot_duration = Duration::from_millis(100);
        let mut clock = SlotClock::from_era_schedule(eras(
            OffsetDateTime::now_utc(),
            slot_duration,
            None,
        ));
        clock.observe_slot(Slot::from(10));
        let anchor = clock.last_observed_at;

        let reached = clock.sleep_until(Slot::from(5)).unwrap();
        assert!(reached.deadline() <= tokio::time::Instant::now());

        let ahead = clock.sleep_until(Slot::from(13)).unwrap();
        assert!(ahead.deadline() >= tokio::time::Instant::from_std(anchor + slot_duration * 3));
    }
}
