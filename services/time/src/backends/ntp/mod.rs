pub mod async_client;

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{Stream, StreamExt as _};
use lb_log_targets::time as log_targets_time;
use lb_time::{Slot, era::EraTimingSchedule};
use lb_utils::bounded_duration::{MinimalBoundedDuration, NANO};
use time::OffsetDateTime;
use tokio::time::{MissedTickBehavior, interval};
use tokio_stream::wrappers::IntervalStream;

use crate::{
    EpochSlotTickStream, SlotTick,
    backends::{
        TimeBackend,
        common::slot_timer,
        ntp::async_client::{AsyncNTPClient, NTPClientSettings},
    },
};

const LOG_TARGET: &str = log_targets_time::ntp::ROOT;

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
    /// The furthest the servers' time may be from the system clock for the
    /// backend to follow it
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub max_offset: Duration,
}

#[derive(Clone, Debug)]
pub struct NtpTimeBackend {
    settings: NtpTimeBackendSettings,
    era_schedule: EraTimingSchedule,
    client: AsyncNTPClient,
}

impl TimeBackend for NtpTimeBackend {
    type Settings = NtpTimeBackendSettings;

    fn init(settings: Self::Settings, era_schedule: EraTimingSchedule) -> Self {
        let client = AsyncNTPClient::new(settings.ntp_client_settings);
        Self {
            settings,
            era_schedule,
            client,
        }
    }

    fn tick_stream(self) -> (SlotTick, EpochSlotTickStream) {
        let Self {
            settings,
            era_schedule,
            client,
        } = self;
        // compute the initial slot ticking stream
        let era_schedules = Arc::new(era_schedule);
        let current_time = OffsetDateTime::now_utc();
        let current_slot = era_schedules
            .slot_at(current_time)
            .unwrap_or(Slot::genesis());
        let (current_slot_tick, slot_timer) =
            slot_timer(Arc::clone(&era_schedules), current_time, current_slot);
        (
            current_slot_tick,
            Pin::new(Box::new(NtpStream {
                readings: ntp_readings(client, &settings),
                era_schedules,
                slot_timer,
                last_emitted_slot: current_slot_tick.slot,
                max_offset: settings.max_offset,
            })),
        )
    }
}

/// The time a round of NTP requests settled on, as an offset from the system
/// clock.
#[derive(Clone, Copy, Debug)]
struct NtpReading {
    /// The system clock when the round ended.
    system_time: OffsetDateTime,
    /// How far ahead of the system clock the servers' time was.
    offset: time::Duration,
}

type NtpReadingStream = Pin<Box<dyn Stream<Item = NtpReading> + Send + Sync + Unpin>>;

/// The time the NTP servers of `settings` tell through `client`, asked right
/// away and then at every update interval.
fn ntp_readings(client: AsyncNTPClient, settings: &NtpTimeBackendSettings) -> NtpReadingStream {
    let mut update_interval = interval(settings.update_interval);
    // if we miss a tick just try next one
    update_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // contact the ntp server for first time sync right now
    let ntp_server = settings.ntp_server.clone();
    Pin::new(Box::new(
        IntervalStream::new(update_interval)
            .zip(futures::stream::repeat((client, ntp_server)))
            .filter_map(move |(_, (client, ntp_server))| {
                Box::pin(async move {
                    match client.request_offset(ntp_server.clone()).await {
                        Ok(offset) => Some(NtpReading {
                            system_time: OffsetDateTime::now_utc(),
                            offset,
                        }),
                        Err(e) => {
                            tracing::warn!(
                                target: LOG_TARGET,
                                "NTP sync failed from {ntp_server}: {e}"
                            );
                            None
                        }
                    }
                })
            }),
    ))
}

/// Stream that updates itself from the NTP servers at every update interval.
pub struct NtpStream {
    /// The time of the NTP servers, read at every update interval
    readings: NtpReadingStream,
    /// The chain's eras, which lay slots and epochs out in time.
    era_schedules: Arc<EraTimingSchedule>,
    /// `SlotTick` interval stream. This stream is replaced when an internal
    /// clock update happens.
    slot_timer: EpochSlotTickStream,
    /// `last_emitted` preserves externally visible monotonicity across NTP
    /// corrections, including backward corrections that must pause emission
    /// until corrected time catches up.
    last_emitted_slot: Slot,
    /// The furthest a reading may be from the system clock to be followed.
    max_offset: Duration,
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
    /// Rebase the internal slot timer to the most recent NTP-derived slot.
    ///
    /// Policy:
    /// - A reading further than `max_offset` from the system clock is skipped
    ///   and leaves the timer as it is. No reading can then take the clock far
    ///   from the system clock, nor pause it for long once a backward
    ///   correction undoes a forward one.
    /// - Backward NTP corrections never move `last_emitted_slot` backwards. We
    ///   replace the synthetic timer and pause emission until corrected time
    ///   reaches a slot strictly greater than the last emitted one.
    /// - Forward NTP corrections replace the synthetic timer and publish the
    ///   corrected slot at once, rather than at the next slot boundary. This
    ///   can skip intermediate slot numbers if NTP jumps ahead.
    ///
    /// The corrected slot leads the new timer either way:
    /// [`Self::poll_slot_timer`] emits only slots past the last emitted one,
    /// which is what tells the cases apart.
    fn handle_ntp_update(self: Pin<&mut Self>, cx: &mut Context<'_>) {
        let this = self.get_mut();

        let Poll::Ready(Some(NtpReading {
            system_time,
            offset,
        })) = this.readings.as_mut().poll_next_unpin(cx)
        else {
            return;
        };
        let Some(date) = system_time
            .checked_add(offset)
            .filter(|_| offset.unsigned_abs() <= this.max_offset)
        else {
            tracing::warn!(
                target: LOG_TARGET,
                "Skipping NTP time {offset} off the system clock, more than the {:?} allowed",
                this.max_offset
            );
            return;
        };

        let current_slot = this.era_schedules.slot_at(date).unwrap_or(Slot::genesis());
        let (current_slot_tick, new_slot_timer) =
            slot_timer(Arc::clone(&this.era_schedules), date, current_slot);
        let rebased_slot_timer =
            Box::pin(futures::stream::iter([current_slot_tick]).chain(new_slot_timer));

        if current_slot < this.last_emitted_slot {
            tracing::warn!(
                target: LOG_TARGET,
                "NTP resync moved backwards: computed_slot={current_slot:?}, \
                last_emitted_slot={:?}; pausing until NTP catches up",
                this.last_emitted_slot
            );
            this.slot_timer = rebased_slot_timer;
            return;
        }

        tracing::trace!(
            target: LOG_TARGET,
            "Applying NTP clock update for slot {current_slot:?}, {offset} off the system clock"
        );

        this.slot_timer = rebased_slot_timer;
    }

    // Emit only strictly increasing slots. After a backward NTP correction this
    // keeps the rebased timer paused until corrected time catches up; after a
    // forward correction it emits the corrected slot at once and resumes from
    // the new NTP-derived timeline, potentially skipping slots.
    fn poll_slot_timer(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<SlotTick>> {
        let this = self.as_mut().get_mut();
        loop {
            match this.slot_timer.as_mut().poll_next_unpin(cx) {
                // Clamp slot to never go backwards. The timer is polled again,
                // so that it wakes the task up for its next tick: a tick it has
                // just returned leaves no wake-up behind.
                Poll::Ready(Some(tick)) if tick.slot <= this.last_emitted_slot => {}
                Poll::Ready(Some(tick)) => {
                    this.last_emitted_slot = tick.slot;
                    return Poll::Ready(Some(tick));
                }
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use lb_time::{
        Era,
        era::{EraEntriesAfterGenesis, EraTiming},
    };

    use super::*;

    /// The furthest a reading may be from the system clock in these tests.
    const MAX_OFFSET: Duration = Duration::from_secs(30);

    /// One era from the Unix epoch, with slots of 1 s in epochs of 3 slots.
    fn test_eras() -> EraTimingSchedule {
        EraTimingSchedule::new(
            OffsetDateTime::UNIX_EPOCH,
            EraTiming {
                slot_duration: Duration::from_secs(1),
                epoch_length: NonZero::new(3).unwrap(),
            },
            EraEntriesAfterGenesis::empty(),
        )
        .unwrap()
    }

    /// A reading of servers that agree with the system clock, `seconds` after
    /// the Unix epoch.
    fn reading_at(seconds: f64) -> NtpReading {
        NtpReading {
            system_time: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds_f64(seconds),
            offset: time::Duration::ZERO,
        }
    }

    enum MockNtpReadingStep {
        Yield(NtpReading),
        Pending,
    }

    struct MockNtpReadingStepStream {
        steps: Vec<MockNtpReadingStep>,
        idx: usize,
    }
    impl Stream for MockNtpReadingStepStream {
        type Item = NtpReading;
        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if self.idx >= self.steps.len() {
                return Poll::Ready(None);
            }
            let poll = match self.steps.get(self.idx) {
                Some(MockNtpReadingStep::Yield(reading)) => Poll::Ready(Some(*reading)),
                Some(MockNtpReadingStep::Pending) => Poll::Pending,
                None => Poll::Ready(None),
            };
            self.idx += 1;
            poll
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
                era: Era::genesis(),
                epoch: 0.into(),
                slot: self.slot,
            }))
        }
    }

    struct MockSlotTimerSequence {
        slots: Vec<Slot>,
        idx: usize,
    }
    impl Stream for MockSlotTimerSequence {
        type Item = SlotTick;
        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if self.idx < self.slots.len() {
                let slot = self.slots[self.idx];
                self.idx += 1;
                Poll::Ready(Some(SlotTick {
                    era: Era::genesis(),
                    epoch: 0.into(),
                    slot,
                }))
            } else {
                Poll::Pending
            }
        }
    }

    /// Helper to create and poll an `NtpStream` for a given test scenario.
    fn check_monotonic_slots(readings: Vec<NtpReading>, initial_slot: u64) {
        let poll_count = readings.len();
        let slot_timer = MockSlotTimer {
            slot: Slot::new(initial_slot),
        };
        let mut stream = NtpStream {
            readings: Box::pin(futures::stream::iter(readings)),
            era_schedules: Arc::new(test_eras()),
            slot_timer: Box::pin(slot_timer),
            last_emitted_slot: Slot::new(initial_slot),
            max_offset: MAX_OFFSET,
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

    fn ntp_data_set_backward_ntp_update() -> (Vec<NtpReading>, u64) {
        (
            [11.023, 12.047, 9.07, 13.093, 8.116, 14.14]
                .map(reading_at)
                .to_vec(),
            10,
        )
    }

    #[tokio::test]
    async fn test_monotonicity_on_backward_ntp_update() {
        // Simulate NTP readings that go back and forth around the initial slot
        let (readings, initial_slot) = ntp_data_set_backward_ntp_update();
        check_monotonic_slots(readings, initial_slot);
    }
    fn ntp_data_set_stalled_ntp() -> (Vec<NtpReading>, u64) {
        ([6.0, 6.023, 7.0, 7.047, 8.0].map(reading_at).to_vec(), 5)
    }

    #[tokio::test]
    async fn test_no_duplicate_slots_on_stalled_ntp() {
        let (readings, initial_slot) = ntp_data_set_stalled_ntp();
        check_monotonic_slots(readings, initial_slot);
    }

    fn ntp_data_set_forward_and_backward_jumps() -> (Vec<NtpReading>, u64) {
        (
            [10.0, 11.023, 9.047, 12.0, 8.0].map(reading_at).to_vec(),
            10,
        )
    }

    #[tokio::test]
    async fn test_forward_and_backward_jumps_ntp() {
        let (readings, initial_slot) = ntp_data_set_forward_and_backward_jumps();
        check_monotonic_slots(readings, initial_slot);
    }

    #[tokio::test(start_paused = true)]
    async fn test_backward_ntp_update_rebases_ahead_slot_stream() {
        let mut stream = NtpStream {
            readings: Box::pin(futures::stream::iter([reading_at(7.0)])),
            era_schedules: Arc::new(test_eras()),
            slot_timer: Box::pin(MockSlotTimerSequence {
                slots: vec![Slot::new(11)],
                idx: 0,
            }),
            last_emitted_slot: Slot::new(10),
            max_offset: MAX_OFFSET,
        };

        // Step 1: consume a backward NTP correction to slot 7.
        // Expected: the old synthetic `11` is discarded and nothing is emitted
        // yet.
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 2: corrected time has only reached synthetic slot 8.
        // Expected: still paused because 8 <= last_emitted_slot (10).
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 3: corrected time has reached slot 9.
        // Expected: still paused because 9 <= 10.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 4: corrected time has reached slot 10.
        // Expected: still paused because duplicates are suppressed (`<=
        // last_emitted_slot`).
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 5: corrected time reaches slot 11.
        // Expected: emission resumes at the first slot strictly greater than
        // 10.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { slot, .. })) if slot == Slot::new(11)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_tick_does_not_stall_the_stream() {
        // After a backward correction, the timer catches up through slots 8 to
        // 10, which were already emitted. A tick it has just returned leaves no
        // wake-up behind, so the stream must poll it again rather than wait.
        let mut stream = NtpStream {
            readings: Box::pin(futures::stream::empty::<NtpReading>()),
            era_schedules: Arc::new(test_eras()),
            slot_timer: Box::pin(MockSlotTimerSequence {
                slots: [8, 9, 10, 11].map(Slot::new).to_vec(),
                idx: 0,
            }),
            last_emitted_slot: Slot::new(10),
            max_offset: MAX_OFFSET,
        };

        let tick = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
        assert!(
            matches!(tick, Ok(Some(SlotTick { slot, .. })) if slot == Slot::new(11)),
            "{tick:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_forward_ntp_update_skips_to_rebased_slot_stream() {
        let mut stream = NtpStream {
            readings: Box::pin(futures::stream::iter([reading_at(20.0)])),
            era_schedules: Arc::new(test_eras()),
            slot_timer: Box::pin(MockSlotTimerSequence {
                slots: vec![Slot::new(11)],
                idx: 0,
            }),
            last_emitted_slot: Slot::new(10),
            max_offset: MAX_OFFSET,
        };

        // Step 1: consume a forward NTP correction to slot 20.
        // Expected: the stale synthetic `11` is discarded and the corrected
        // slot 20 is emitted at once, skipping 11..19.
        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { slot, .. })) if slot == Slot::new(20)
        ));

        // Step 2: one slot later, the rebased timer reaches slot 21.
        // Expected: emission goes on along the rebased timeline.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { slot, .. })) if slot == Slot::new(21)
        ));
    }

    /// A reading far from the system clock, such as a forged one 50 years
    /// ahead, is skipped. The servers that tell the time after it set the
    /// clock, rather than the clock freezing until real time reaches the
    /// forged one.
    #[tokio::test(start_paused = true)]
    async fn a_reading_far_from_the_system_clock_is_skipped() {
        let system_time = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10);
        let mut stream = NtpStream {
            readings: Box::pin(futures::stream::iter([
                NtpReading {
                    system_time,
                    offset: time::Duration::days(50 * 365),
                },
                NtpReading {
                    system_time,
                    offset: time::Duration::SECOND,
                },
            ])),
            era_schedules: Arc::new(test_eras()),
            slot_timer: Box::pin(MockSlotTimerSequence {
                slots: vec![],
                idx: 0,
            }),
            last_emitted_slot: Slot::new(10),
            max_offset: MAX_OFFSET,
        };

        // Step 1: consume the reading 50 years ahead.
        // Expected: it is skipped, so nothing is emitted.
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 2: consume a reading a second ahead of the system clock.
        // Expected: the corrected slot 11 is emitted at once.
        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { slot, .. })) if slot == Slot::new(11)
        ));

        // Step 3: one slot later, the rebased timer reaches slot 12.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { slot, .. })) if slot == Slot::new(12)
        ));
    }

    /// A forward correction into a new era publishes the corrected slot at
    /// once, in the new era, so the era is not entered a slot late.
    #[tokio::test(start_paused = true)]
    async fn a_forward_correction_into_a_new_era_publishes_it_at_once() {
        // Era 1 starts at epoch 34, which is slot 102 with epochs of 3 slots.
        let timing = EraTiming {
            slot_duration: Duration::from_secs(1),
            epoch_length: NonZero::new(3).unwrap(),
        };
        let mut stream = NtpStream {
            readings: Box::pin(futures::stream::iter([reading_at(104.0)])),
            era_schedules: Arc::new(
                EraTimingSchedule::new(
                    OffsetDateTime::UNIX_EPOCH,
                    timing,
                    EraEntriesAfterGenesis::from((NonZero::new(34).unwrap(), timing)),
                )
                .unwrap(),
            ),
            slot_timer: Box::pin(MockSlotTimerSequence {
                slots: vec![],
                idx: 0,
            }),
            last_emitted_slot: Slot::new(99),
            max_offset: MAX_OFFSET,
        };

        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { era, slot, .. }))
                if slot == Slot::new(104) && era == Era::new(1)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn test_on_time_forward_on_time_backward_ntp_sequence() {
        let mut stream = NtpStream {
            readings: Box::pin(MockNtpReadingStepStream {
                steps: vec![
                    MockNtpReadingStep::Yield(reading_at(10.0)),
                    MockNtpReadingStep::Yield(reading_at(20.0)),
                    MockNtpReadingStep::Pending,
                    MockNtpReadingStep::Yield(reading_at(21.0)),
                    MockNtpReadingStep::Pending,
                    MockNtpReadingStep::Yield(reading_at(19.0)),
                    MockNtpReadingStep::Pending,
                    MockNtpReadingStep::Pending,
                    MockNtpReadingStep::Pending,
                    MockNtpReadingStep::Pending,
                ],
                idx: 0,
            }),
            era_schedules: Arc::new(test_eras()),
            slot_timer: Box::pin(MockSlotTimerSequence {
                slots: vec![Slot::new(11)],
                idx: 0,
            }),
            last_emitted_slot: Slot::new(10),
            max_offset: MAX_OFFSET,
        };

        // Step 1: consume an on-time NTP update for slot 10.
        // Expected: the timer is rebased to the same slot and still emits
        // nothing immediately.
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 2: consume a forward NTP update to slot 20.
        // Expected: the old synthetic `11` is discarded and the corrected slot
        // 20 is emitted at once.
        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { slot, .. })) if slot == Slot::new(20)
        ));

        // Step 3: no NTP update on this poll; one second of simulated time
        // passes. Expected: the rebased forward timer now emits slot
        // 21.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { slot, .. })) if slot == Slot::new(21)
        ));

        // Step 4: consume another on-time NTP update for slot 21.
        // Expected: the stream is rebased to 21 and does not duplicate slot 21
        // immediately.
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 5: no NTP update on this poll; one second of simulated time
        // passes. Expected: the rebased on-time timer emits slot 22.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { slot, .. })) if slot == Slot::new(22)
        ));

        // Step 6: consume a backward NTP update to slot 19.
        // Expected: no regression is emitted; the stream pauses at
        // last_emitted_slot = 22.
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 7: corrected time reaches slot 20.
        // Expected: still paused because 20 <= 22.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 8: corrected time reaches slot 21.
        // Expected: still paused because 21 <= 22.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 9: corrected time reaches slot 22.
        // Expected: still paused because duplicates are suppressed.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(futures::poll!(stream.next()), Poll::Pending));

        // Step 10: corrected time reaches slot 23.
        // Expected: emission resumes at the first slot strictly greater than
        // 22.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            futures::poll!(stream.next()),
            Poll::Ready(Some(SlotTick { slot, .. })) if slot == Slot::new(23)
        ));
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
            max_offset: MAX_OFFSET,
        };
        poll_ntp_backend_stream(backend_settings, 25).await;
        // If we reach here, tick_stream handled polling and edge cases robustly
    }

    #[tokio::test]
    // Fast polling will pick up jitter from the real NTP server, testing
    // real-world monotonicity and robustness of the tick stream
    async fn test_real_ntp_server_fast_poll() {
        let backend_settings = NtpTimeBackendSettings {
            ntp_server: "pool.ntp.org:123".to_owned(),
            ntp_client_settings: NTPClientSettings {
                timeout: Duration::from_secs(2),
                listening_interface: "0.0.0.0".parse().unwrap(),
            },
            update_interval: Duration::from_millis(50),
            max_offset: MAX_OFFSET,
        };
        poll_ntp_backend_stream(backend_settings, 25).await;
        // If we reach here, tick_stream handled polling and edge cases robustly
    }

    async fn poll_ntp_backend_stream(backend_settings: NtpTimeBackendSettings, poll_count: u8) {
        let backend = NtpTimeBackend::init(backend_settings, test_eras());
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
