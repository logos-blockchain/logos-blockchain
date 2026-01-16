use cryptarchia_engine::Slot;
use time::OffsetDateTime;

use crate::{
    EpochSlotTickStream, SlotTick, TimeServiceSettings,
    backends::{TimeBackend, common::slot_timer},
};

pub struct SystemTimeBackend {
    settings: TimeServiceSettings<()>,
}

impl TimeBackend for SystemTimeBackend {
    type Settings = ();

    fn init(settings: TimeServiceSettings<Self::Settings>) -> Self {
        Self { settings }
    }

    fn tick_stream(self) -> (SlotTick, EpochSlotTickStream) {
        let Self { settings } = self;
        let local_date = OffsetDateTime::now_utc();
        let current_slot = Slot::from_offset_and_config(local_date, settings.slot_config);
        slot_timer(
            settings.slot_config,
            local_date,
            current_slot,
            settings.epoch_config,
            settings.base_period_length,
        )
    }
}

#[cfg(test)]
mod test {
    use std::{num::NonZero, time::Duration};

    use cryptarchia_engine::{EpochConfig, Slot, time::SlotConfig};
    use futures::StreamExt as _;
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
        let settings = TimeServiceSettings {
            slot_config: SlotConfig {
                slot_duration: Duration::from_secs(1),
                chain_start_time: OffsetDateTime::now_utc(),
            },
            epoch_config: EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(3).unwrap(),
                epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
            },
            base_period_length: NonZero::new(10).unwrap(),
            backend: (),
        };
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
