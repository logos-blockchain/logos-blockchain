pub mod async_client;

use std::{
    num::NonZero,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::{Stream, StreamExt as _};
use lb_cryptarchia_engine::{EpochConfig, Slot, time::SlotConfig};
use lb_utils::bounded_duration::{MinimalBoundedDuration, NANO};
use sntpc::{NtpResult, fraction_to_nanoseconds};
use time::OffsetDateTime;
use tokio::time::{MissedTickBehavior, interval};
use tokio_stream::wrappers::IntervalStream;

use crate::{
    EpochSlotTickStream, SlotTick, TimeServiceSettings,
    backends::{
        TimeBackend,
        common::slot_timer,
        ntp::async_client::{AsyncNTPClient, NTPClientSettings},
    },
};

#[serde_with::serde_as]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NtpTimeBackendSettings {
    /// Ntp server address
    pub ntp_server: String,
    /// Ntp server settings
    pub ntp_client_settings: NTPClientSettings,
    /// Interval for the backend to contact the ntp server and update its time
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub update_interval: Duration,
}

#[derive(Clone, Debug)]
pub struct NtpTimeBackend {
    settings: TimeServiceSettings<NtpTimeBackendSettings>,
    client: AsyncNTPClient,
}

impl TimeBackend for NtpTimeBackend {
    type Settings = NtpTimeBackendSettings;

    fn init(settings: TimeServiceSettings<Self::Settings>) -> Self {
        let client = AsyncNTPClient::new(settings.backend.ntp_client_settings);
        Self { settings, client }
    }

    fn tick_stream(self) -> (SlotTick, EpochSlotTickStream) {
        let Self { settings, client } = self;
        let mut update_interval = interval(settings.backend.update_interval);
        // if we miss a tick just try next one
        update_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // contact the ntp server for first time sync right now
        let ntp_server = settings.backend.ntp_server.clone();
        let interval: NtpResultStream = Pin::new(Box::new(
            IntervalStream::new(update_interval)
                .zip(futures::stream::repeat((client, ntp_server)))
                .filter_map(move |(_, (client, ntp_server))| {
                    Box::pin(async move {
                        match client.request_timestamp(ntp_server.clone()).await {
                            Ok(result) => Some(result),
                            Err(e) => {
                                tracing::warn!("NTP sync failed from {ntp_server}: {e}");
                                None
                            }
                        }
                    })
                }),
        ));
        // compute the initial slot ticking stream
        let local_date = OffsetDateTime::now_utc();
        let (current_slot_tick, slot_timer) = slot_timer(
            settings.slot_config,
            local_date,
            Slot::from_offset_and_config(local_date, settings.slot_config),
            settings.epoch_config,
            settings.base_period_length,
        );
        (
            current_slot_tick,
            Pin::new(Box::new(NtpStream {
                interval,
                slot_config: settings.slot_config,
                epoch_config: settings.epoch_config,
                base_period_length: settings.base_period_length,
                slot_timer,
                last_emitted_slot: current_slot_tick.slot,
            })),
        )
    }
}

type NtpResultStream = Pin<Box<dyn Stream<Item = NtpResult> + Send + Sync + Unpin>>;

/// Stream that updates itself every `interval` from an NTP server.
pub struct NtpStream {
    /// Update interval stream
    interval: NtpResultStream,
    /// Slot settings in order to compute proper slot times
    slot_config: SlotConfig,
    /// Epoch settings in order to compute proper epoch times
    epoch_config: EpochConfig,
    /// Base period length related to epochs, used to compute epochs as well
    base_period_length: NonZero<u64>,
    /// `SlotTick` interval stream. This stream is replaced when an internal
    /// clock update happens.
    slot_timer: EpochSlotTickStream,
    /// `last_emitted` is used to detect if the new computed slot after an NTP
    /// update goes backwards, which can happen if the NTP server time is
    /// not monotonic. In that case we clamp the new slot to the last
    /// emitted one and log a warning.
    last_emitted_slot: Slot,
}
impl Stream for NtpStream {
    type Item = SlotTick;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Try update time
        self.as_mut().handle_ntp_update(cx);
        // Poll from internal last updated `SlotTick` stream
        self.as_mut().poll_slot_timer(cx)
    }
}

impl NtpStream {
    #[expect(
        clippy::cognitive_complexity,
        reason = "Keep NTP update flow local in this PR"
    )]
    fn handle_ntp_update(self: Pin<&mut Self>, cx: &mut Context<'_>) {
        let this = self.get_mut();

        let Poll::Ready(Some(timestamp)) = this.interval.as_mut().poll_next_unpin(cx) else {
            return;
        };

        let seconds = Duration::from_secs(timestamp.sec().into());
        let nanos_fraction =
            Duration::from_nanos(fraction_to_nanoseconds(timestamp.sec_fraction()).into());
        let roundtrip = Duration::from_micros(timestamp.roundtrip());
        let ts_nanos_u128 = (seconds + nanos_fraction + roundtrip / 2).as_nanos();

        let ts_nanos_i128 = match i128::try_from(ts_nanos_u128) {
            Ok(ts_nanos) => ts_nanos,
            Err(e) => {
                tracing::warn!(
                    "Skipping invalid NTP timestamp {ts_nanos_u128} vs. {}: {e}",
                    i128::MAX
                );
                return;
            }
        };

        let date = match OffsetDateTime::from_unix_timestamp_nanos(ts_nanos_i128) {
            Ok(date) => date,
            Err(e) => {
                tracing::warn!("Skipping invalid NTP timestamp: {e:?} (ts_nanos={ts_nanos_i128})");
                return;
            }
        };

        let current_slot = Slot::from_offset_and_config(date, this.slot_config);
        if current_slot < this.last_emitted_slot {
            tracing::warn!(
                "NTP resync moved backwards: computed_slot={current_slot:?}, \
                last_emitted_slot={:?}; clamping",
                this.last_emitted_slot
            );
            return;
        }
        tracing::trace!(
            "Applying NTP clock update for slot {current_slot:?} with roundtrip {}us",
            roundtrip.as_micros()
        );

        let epoch_config = this.epoch_config;
        let base_period_length = this.base_period_length;
        (_, this.slot_timer) = slot_timer(
            this.slot_config,
            date,
            current_slot,
            epoch_config,
            base_period_length,
        );
        this.last_emitted_slot = current_slot;
    }

    // Polls the slot_timer and clamps slot to never go backwards.
    fn poll_slot_timer(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<SlotTick>> {
        let this = self.as_mut().get_mut();
        match this.slot_timer.as_mut().poll_next_unpin(cx) {
            Poll::Ready(Some(tick)) => {
                // Clamp slot to never go backwards
                if tick.slot < this.last_emitted_slot {
                    return Poll::Pending;
                }
                this.last_emitted_slot = tick.slot;
                Poll::Ready(Some(tick))
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    // Dummy SlotConfig and EpochConfig for testing
    fn test_configs() -> (SlotConfig, EpochConfig, NonZeroU64) {
        let slot_config = SlotConfig {
            slot_duration: Duration::from_secs(1),
            genesis_time: OffsetDateTime::UNIX_EPOCH,
        };
        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(1).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(1).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(1).unwrap(),
        };
        let base_period_length = NonZeroU64::new(1).unwrap();
        (slot_config, epoch_config, base_period_length)
    }

    // Struct to hold richer NTP test data
    #[derive(Clone, Copy)]
    struct NtpTestData {
        sec: u32,
        sec_fraction: u32,
        offset: i32,
        roundtrip: u32,
        stratum: u8,
        leap: u8,
    }

    // Mock NtpResult that simulates a given NTP response
    fn mock_ntp_result(data: NtpTestData) -> NtpResult {
        NtpResult::new(
            data.sec,
            data.sec_fraction,
            data.offset as u64,
            i64::from(data.roundtrip),
            data.stratum,
            data.leap as i8,
        )
    }

    // Mock NtpResultStream that yields a sequence of NtpResults with increasing or
    // decreasing time
    struct MockNtpResultStream {
        data: Vec<NtpTestData>,
        idx: usize,
    }
    impl Stream for MockNtpResultStream {
        type Item = NtpResult;
        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if self.idx < self.data.len() {
                let d = self.data[self.idx];
                self.idx += 1;
                Poll::Ready(Some(mock_ntp_result(d)))
            } else {
                Poll::Ready(None)
            }
        }
    }

    // Mock slot_timer that always yields a SlotTick with the current slot
    struct MockSlotTimer {
        slot: Slot,
    }
    impl Stream for MockSlotTimer {
        type Item = SlotTick;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(Some(SlotTick {
                epoch: 0.into(),
                slot: self.slot,
            }))
        }
    }

    /// Helper to create and poll an `NtpStream` for a given test scenario.
    fn check_monotonic_slots(ntp_data: Vec<NtpTestData>, initial_slot: u64) {
        let poll_count = ntp_data.len();
        let (slot_config, epoch_config, base_period_length) = test_configs();
        let ntp_stream = MockNtpResultStream {
            data: ntp_data,
            idx: 0,
        };
        let slot_timer = MockSlotTimer {
            slot: Slot::new(initial_slot),
        };
        let mut stream = NtpStream {
            interval: Box::pin(ntp_stream),
            slot_config,
            epoch_config,
            base_period_length,
            slot_timer: Box::pin(slot_timer),
            last_emitted_slot: Slot::new(initial_slot),
        };
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        let mut last_slot = initial_slot;
        for _ in 0..poll_count {
            if let Poll::Ready(Some(SlotTick { slot, .. })) =
                Pin::new(&mut stream).poll_next(&mut cx)
            {
                let slot_num = slot.into_inner();
                assert!(
                    slot_num >= last_slot,
                    "Slot regressed from {last_slot} to {slot_num}"
                );
                last_slot = slot_num;
            }
        }
    }

    fn ntp_data_set_backward_ntp_update() -> (Vec<NtpTestData>, u64) {
        (
            vec![
                NtpTestData {
                    sec: 11,
                    sec_fraction: 100_000_000,
                    offset: 10,
                    roundtrip: 100,
                    stratum: 1,
                    leap: 0,
                },
                NtpTestData {
                    sec: 12,
                    sec_fraction: 200_000_000,
                    offset: 20,
                    roundtrip: 200,
                    stratum: 1,
                    leap: 0,
                },
                NtpTestData {
                    sec: 9,
                    sec_fraction: 300_000_000,
                    offset: -30,
                    roundtrip: 300,
                    stratum: 2,
                    leap: 1,
                },
                NtpTestData {
                    sec: 13,
                    sec_fraction: 400_000_000,
                    offset: 40,
                    roundtrip: 400,
                    stratum: 1,
                    leap: 0,
                },
                NtpTestData {
                    sec: 8,
                    sec_fraction: 500_000_000,
                    offset: -50,
                    roundtrip: 500,
                    stratum: 2,
                    leap: 1,
                },
                NtpTestData {
                    sec: 14,
                    sec_fraction: 600_000_000,
                    offset: 60,
                    roundtrip: 600,
                    stratum: 1,
                    leap: 0,
                },
            ],
            10,
        )
    }

    #[tokio::test]
    async fn test_monotonicity_on_backward_ntp_update() {
        // Simulate NTP responses with varying sec_fraction, offset, roundtrip, stratum,
        // leap
        let (ntp_data, initial_slot) = ntp_data_set_backward_ntp_update();
        check_monotonic_slots(ntp_data, initial_slot);
    }
    fn ntp_data_set_stalled_ntp() -> (Vec<NtpTestData>, u64) {
        (
            vec![
                NtpTestData {
                    sec: 6,
                    sec_fraction: 0,
                    offset: 0,
                    roundtrip: 0,
                    stratum: 1,
                    leap: 0,
                },
                NtpTestData {
                    sec: 6,
                    sec_fraction: 100_000_000,
                    offset: 5,
                    roundtrip: 10,
                    stratum: 1,
                    leap: 0,
                },
                NtpTestData {
                    sec: 7,
                    sec_fraction: 0,
                    offset: 0,
                    roundtrip: 0,
                    stratum: 1,
                    leap: 0,
                },
                NtpTestData {
                    sec: 7,
                    sec_fraction: 200_000_000,
                    offset: -5,
                    roundtrip: 20,
                    stratum: 2,
                    leap: 1,
                },
                NtpTestData {
                    sec: 8,
                    sec_fraction: 0,
                    offset: 0,
                    roundtrip: 0,
                    stratum: 1,
                    leap: 0,
                },
            ],
            5,
        )
    }

    #[tokio::test]
    async fn test_no_duplicate_slots_on_stalled_ntp() {
        let (ntp_data, initial_slot) = ntp_data_set_stalled_ntp();
        check_monotonic_slots(ntp_data, initial_slot);
    }

    fn ntp_data_set_forward_and_backward_jumps() -> (Vec<NtpTestData>, u64) {
        (
            vec![
                NtpTestData {
                    sec: 10,
                    sec_fraction: 0,
                    offset: 0,
                    roundtrip: 0,
                    stratum: 1,
                    leap: 0,
                },
                NtpTestData {
                    sec: 11,
                    sec_fraction: 100_000_000,
                    offset: 5,
                    roundtrip: 10,
                    stratum: 1,
                    leap: 0,
                },
                NtpTestData {
                    sec: 9,
                    sec_fraction: 200_000_000,
                    offset: -5,
                    roundtrip: 20,
                    stratum: 2,
                    leap: 1,
                },
                NtpTestData {
                    sec: 12,
                    sec_fraction: 0,
                    offset: 0,
                    roundtrip: 0,
                    stratum: 1,
                    leap: 0,
                },
                NtpTestData {
                    sec: 8,
                    sec_fraction: 0,
                    offset: 0,
                    roundtrip: 0,
                    stratum: 1,
                    leap: 0,
                },
            ],
            10,
        )
    }

    #[tokio::test]
    async fn test_forward_and_backward_jumps_ntp() {
        let (ntp_data, initial_slot) = ntp_data_set_forward_and_backward_jumps();
        check_monotonic_slots(ntp_data, initial_slot);
    }

    #[tokio::test]
    async fn test_dummy_ntp_server_normal_poll() {
        // Custom settings with dummy NTP server and short interval
        let backend_settings = NtpTimeBackendSettings {
            ntp_server: "dummy.pool.ntp.org".to_owned(),
            ntp_client_settings: NTPClientSettings {
                timeout: Duration::from_secs(1),
                listening_interface: "127.0.0.1".parse().unwrap(),
            },
            update_interval: Duration::from_millis(1),
        };
        let slot_config = SlotConfig {
            slot_duration: Duration::from_secs(1),
            genesis_time: OffsetDateTime::UNIX_EPOCH,
        };
        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(1).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(1).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(1).unwrap(),
        };
        let base_period_length = NonZeroU64::new(1).unwrap();
        let settings = TimeServiceSettings {
            slot_config,
            epoch_config,
            base_period_length,
            backend: backend_settings,
        };
        poll_ntp_backend_stream(settings, 25).await;
        // If we reach here, tick_stream handled polling and edge cases robustly
    }

    #[tokio::test]
    // Fast polling will pick up jitter from the real NTP server, testing real-world
    // monotonicity and robustness of the tick stream
    async fn test_real_ntp_server_fast_poll() {
        let backend_settings = NtpTimeBackendSettings {
            ntp_server: "pool.ntp.org:123".to_owned(),
            ntp_client_settings: NTPClientSettings {
                timeout: Duration::from_secs(2),
                listening_interface: "0.0.0.0".parse().unwrap(),
            },
            update_interval: Duration::from_millis(50),
        };
        let slot_config = SlotConfig {
            slot_duration: Duration::from_secs(1),
            genesis_time: OffsetDateTime::UNIX_EPOCH,
        };
        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(1).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(1).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(1).unwrap(),
        };
        let base_period_length = NonZeroU64::new(1).unwrap();
        let settings = TimeServiceSettings {
            slot_config,
            epoch_config,
            base_period_length,
            backend: backend_settings,
        };
        poll_ntp_backend_stream(settings, 25).await;
        // If we reach here, tick_stream handled polling and edge cases robustly
    }

    async fn poll_ntp_backend_stream(
        settings: TimeServiceSettings<NtpTimeBackendSettings>,
        poll_count: u8,
    ) {
        let backend = NtpTimeBackend::init(settings);
        let (_initial_tick, mut stream) = backend.tick_stream();
        let mut previous_tick: Option<SlotTick> = None;
        for _ in 0..poll_count {
            let tick = stream.next().await;
            // println!("tick: {tick:?}");
            if let Some(current) = tick {
                if let Some(previous) = previous_tick {
                    assert!(current.slot.into_inner() > previous.slot.into_inner());
                    assert!(current.epoch.into_inner() >= previous.epoch.into_inner());
                }
                previous_tick = tick;
            }
        }
    }
}
