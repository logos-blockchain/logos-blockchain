use std::sync::Arc;

use lb_time::{Slot, era::EraSchedules};
use time::OffsetDateTime;

use crate::{
    EpochSlotTickStream, SlotTick, TimeServiceSettingsSchedule,
    backends::{TimeBackend, common::slot_timer},
};

pub struct SystemTimeBackend {
    eras: EraSchedules,
}

impl TimeBackend for SystemTimeBackend {
    type Settings = ();

    fn init(settings_schedule: TimeServiceSettingsSchedule<Self::Settings>) -> Self {
        Self {
            eras: settings_schedule.map(|_| ()),
        }
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
        era::{EraEntriesAfterGenesis, EraEntry, EraSchedule},
    };
    use time::OffsetDateTime;

    use crate::{
        TimeServiceSettings,
        backends::{TimeBackend as _, system_time::SystemTimeBackend},
    };

    #[tokio::test]
    async fn test_stream() {
        const SAMPLE_SIZE: u64 = 5;
        // The initial slot is 0 but we expect the stream starts from the next slot (1).
        let expected: Vec<_> = (1..=SAMPLE_SIZE).map(Slot::from).collect();
        let settings = EraSchedule::new(
            OffsetDateTime::now_utc(),
            EraEntry {
                slot_duration: Duration::from_secs(1),
                epoch_length_in_slots: NonZero::new(100).unwrap(),
                parameters: TimeServiceSettings { backend: () },
            },
            EraEntriesAfterGenesis::empty(),
        )
        .unwrap();
        let backend = SystemTimeBackend::init(settings);
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
