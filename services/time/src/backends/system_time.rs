use std::sync::Arc;

use lb_time::{Slot, era::EraTimingSchedule};
use time::OffsetDateTime;

use crate::{
    EpochSlotTickStream, SlotTick,
    backends::{TimeBackend, common::slot_timer},
};

pub struct SystemTimeBackend {
    eras: EraTimingSchedule,
}

impl TimeBackend for SystemTimeBackend {
    type Settings = ();

    fn init((): Self::Settings, era_schedule: EraTimingSchedule) -> Self {
        Self { eras: era_schedule }
    }

    fn tick_stream(self) -> (SlotTick, EpochSlotTickStream) {
        let Self { eras } = self;
        let local_date = OffsetDateTime::now_utc();
        let current_slot = eras.slot_at(local_date).unwrap_or(Slot::genesis());
        slot_timer(Arc::new(eras), local_date, current_slot)
    }
}

#[cfg(test)]
mod test {
    use std::{num::NonZero, time::Duration};

    use futures::StreamExt as _;
    use lb_time::{
        Slot,
        era::{EraEntriesAfterGenesis, EraSchedule, EraTiming},
    };
    use time::OffsetDateTime;

    use crate::backends::{TimeBackend as _, system_time::SystemTimeBackend};

    #[tokio::test]
    async fn test_stream() {
        const SAMPLE_SIZE: u64 = 5;
        // The initial slot is 0 but we expect the stream starts from the next slot (1).
        let expected: Vec<_> = (1..=SAMPLE_SIZE).map(Slot::from).collect();
        let era_schedule = EraSchedule::new(
            OffsetDateTime::now_utc(),
            EraTiming {
                slot_duration: Duration::from_secs(1),
                epoch_length: NonZero::new(100).unwrap(),
            },
            EraEntriesAfterGenesis::empty(),
        )
        .unwrap();
        let backend = SystemTimeBackend::init((), era_schedule);
        let (current_slot_tick, stream) = backend.tick_stream();
        assert_eq!(current_slot_tick.slot, 0.into());
        let result: Vec<_> = stream
            .take(SAMPLE_SIZE as usize)
            .map(|slot_tick| slot_tick.slot)
            .collect()
            .await;
        assert_eq!(expected, result);
    }
}
