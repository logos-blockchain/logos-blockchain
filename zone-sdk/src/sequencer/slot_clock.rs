use std::time::{Duration, Instant, SystemTime};

use lb_common_http_client::Slot;

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
