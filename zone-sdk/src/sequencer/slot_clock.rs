use std::time::{Duration, Instant, SystemTime};

use lb_common_http_client::Slot;

/// Slack after a slot boundary so a wake-up lands inside the new slot; tokio
/// timers tick at millisecond granularity.
const BOUNDARY_GRACE: Duration = Duration::from_millis(10);

#[derive(Clone, Debug)]
pub(super) struct SlotClock {
    slot_duration: Duration,
    chain_start_time: SystemTime,
    last_observed_slot: Slot,
    last_observed_at: Instant,
}

impl SlotClock {
    pub(super) fn from_chain_start_time(
        chain_start_time: SystemTime,
        slot_duration: Duration,
    ) -> Self {
        let current_slot = slot_from_u64(
            SystemTime::now()
                .duration_since(chain_start_time)
                .ok()
                .map_or(0, |elapsed| slots_from_duration(elapsed, slot_duration)),
        );

        Self {
            slot_duration,
            chain_start_time,
            last_observed_slot: current_slot,
            last_observed_at: Instant::now(),
        }
    }

    pub(super) fn observe_slot(&mut self, observed_slot: Slot) {
        self.last_observed_slot = observed_slot;
        self.last_observed_at = Instant::now();
    }

    pub(super) fn current_slot(&self) -> Slot {
        let from_chain_start = SystemTime::now()
            .duration_since(self.chain_start_time)
            .ok()
            .map_or(0, |elapsed| {
                slots_from_duration(elapsed, self.slot_duration)
            });
        let from_anchor = slot_to_u64(self.last_observed_slot).saturating_add(slots_from_duration(
            self.last_observed_at.elapsed(),
            self.slot_duration,
        ));

        slot_from_u64(from_chain_start.max(from_anchor))
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
        let target = slot_to_u64(slot);

        let slots_from_anchor = target.saturating_sub(slot_to_u64(self.last_observed_slot));
        let from_anchor = self
            .last_observed_at
            .checked_add(duration_for_slots(slots_from_anchor, self.slot_duration));

        let from_chain_start = self
            .chain_start_time
            .checked_add(duration_for_slots(target, self.slot_duration))
            .map(|at| system_time_to_instant(at, now));

        // `current_slot` takes the later of its two slot estimates, so the
        // slot is reached at the earlier of the two instants.
        from_anchor
            .into_iter()
            .chain(from_chain_start)
            .min()
            .map(|at| at.max(now))
    }
}

fn system_time_to_instant(at: SystemTime, now: Instant) -> Instant {
    at.duration_since(SystemTime::now())
        .map_or(now, |until| now + until)
}

fn duration_for_slots(slots: u64, slot_duration: Duration) -> Duration {
    let nanos = u128::from(slots) * slot_duration.as_nanos();
    u64::try_from(nanos).map_or(Duration::MAX, Duration::from_nanos)
}

const fn slots_from_duration(elapsed: Duration, slot_duration: Duration) -> u64 {
    let divisor = slot_duration.as_nanos();
    if divisor == 0 {
        return 0;
    }
    let slots = elapsed.as_nanos() / divisor;
    if slots > u64::MAX as u128 {
        u64::MAX
    } else {
        slots as u64
    }
}

pub(super) const fn slot_to_u64(slot: Slot) -> u64 {
    slot.into_inner()
}

fn slot_from_u64(value: u64) -> Slot {
    Slot::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instant_of_uses_the_earlier_of_anchor_and_chain_start() {
        let slot_duration = Duration::from_millis(100);
        let mut clock = SlotClock::from_chain_start_time(SystemTime::now(), slot_duration);
        clock.observe_slot(Slot::from(10));
        let anchor = clock.last_observed_at;

        let at = clock.instant_of(Slot::from(13)).unwrap();
        assert!(at >= anchor + slot_duration * 3);
        assert!(at < anchor + slot_duration * 4);

        assert!(clock.instant_of(Slot::from(5)).unwrap() <= Instant::now());
    }

    #[tokio::test]
    async fn sleep_until_a_reached_slot_does_not_wait() {
        let slot_duration = Duration::from_millis(100);
        let mut clock = SlotClock::from_chain_start_time(SystemTime::now(), slot_duration);
        clock.observe_slot(Slot::from(10));
        let anchor = clock.last_observed_at;

        let reached = clock.sleep_until(Slot::from(5)).unwrap();
        assert!(reached.deadline() <= tokio::time::Instant::now());

        let ahead = clock.sleep_until(Slot::from(13)).unwrap();
        assert!(ahead.deadline() >= tokio::time::Instant::from_std(anchor + slot_duration * 3));
    }
}
