use std::{
    ops::RangeInclusive,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt as _};
use tracing::debug;

const LOG_TARGET: &str = "blend::network::core::core::conn::maintenance";

/// Counts the number of messages received from a peer during
/// an interval.
///
/// Upon each interval conclusion, the provider is expected to return the range
/// of expected messages for the new interval, which is then used by the monitor
/// to evaluate the remote peer during the next observation window.
pub struct ConnectionMonitor<ConnectionWindowClock> {
    expected_message_range: Option<RangeInclusive<u64>>,
    connection_window_clock: ConnectionWindowClock,
    current_window_message_count: u64,
}

/// A result of connection monitoring during an interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMonitorOutput {
    Spammy,
    Unhealthy,
    Healthy,
}

impl<ConnectionWindowClock> ConnectionMonitor<ConnectionWindowClock> {
    pub const fn new(connection_window_clock: ConnectionWindowClock) -> Self {
        Self {
            connection_window_clock,
            current_window_message_count: 0,
            expected_message_range: None,
        }
    }

    /// Record a message received from the peer.
    pub fn record_message(&mut self) {
        self.current_window_message_count = self
            .current_window_message_count
            .checked_add(1)
            .unwrap_or_else(|| {
                tracing::warn!(target: LOG_TARGET, "Skipping recording a message due to overflow");
                self.current_window_message_count
            });
    }

    const fn reset(&mut self, new_expected_message_count_range: RangeInclusive<u64>) {
        self.current_window_message_count = 0;
        self.expected_message_range = Some(new_expected_message_count_range);
    }

    /// Check if the peer is spammy based on the number of messages sent
    const fn is_spammy(&self) -> bool {
        let Some(expected_message_range) = &self.expected_message_range else {
            return false;
        };
        self.current_window_message_count > *expected_message_range.end()
    }

    /// Check if the peer is unhealthy based on the number of messages sent
    const fn is_unhealthy(&self) -> bool {
        let Some(expected_message_range) = &self.expected_message_range else {
            return false;
        };
        self.current_window_message_count < *expected_message_range.start()
    }
}

impl<ConnectionWindowClock> ConnectionMonitor<ConnectionWindowClock>
where
    ConnectionWindowClock: Stream<Item = RangeInclusive<u64>> + Unpin,
{
    /// Poll the connection monitor to check if the interval has elapsed.
    /// If the interval has elapsed, evaluate the peer's status,
    /// reset the monitor, and return the result as `Poll::Ready`.
    /// If not, return `Poll::Pending`.
    pub fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Option<ConnectionMonitorOutput>> {
        let new_expected_message_count_range = match self
            .connection_window_clock
            .poll_next_unpin(cx)
        {
            Poll::Ready(Some(new_expected_message_count_range)) => new_expected_message_count_range,
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => return Poll::Pending,
        };
        // First tick is used to set the range for the new observation window.
        if self.expected_message_range.is_none() {
            debug!(target: LOG_TARGET, "First tick received. Starting monitoring connection from now on at every tick...");
            self.reset(new_expected_message_count_range);
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        let outcome = if self.is_spammy() {
            ConnectionMonitorOutput::Spammy
        } else if self.is_unhealthy() {
            ConnectionMonitorOutput::Unhealthy
        } else {
            ConnectionMonitorOutput::Healthy
        };
        debug!(target: LOG_TARGET, "Monitor clock. Received messages = {:#?}, expected range = {:#?} -> Outcome = {outcome:?}.", self.current_window_message_count, self.expected_message_range);
        self.reset(new_expected_message_count_range);
        Poll::Ready(Some(outcome))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        task::{Context, Poll},
        time::Duration,
    };

    use futures::task::noop_waker;
    use tokio_stream::StreamExt as _;

    use crate::core::with_core::behaviour::handler::conn_maintenance::{
        ConnectionMonitor, ConnectionMonitorOutput,
    };

    #[test]
    fn monitor() {
        // We set the monitor to expect between 1 and 2 messages at each round.
        let mut monitor = ConnectionMonitor::new(futures::stream::iter(std::iter::repeat(1..=2)));

        // First poll should return `Pending` since the expected range has not yet been
        // set.
        assert_eq!(
            monitor.poll(&mut Context::from_waker(&noop_waker())),
            Poll::Pending
        );

        // Recording the minimum expected number of messages,
        // expecting the peer to be healthy
        monitor.record_message();
        assert_eq!(
            monitor.poll(&mut Context::from_waker(&noop_waker())),
            Poll::Ready(Some(ConnectionMonitorOutput::Healthy))
        );

        // Recording the maximum expected number of messages,
        // expecting the peer to be healthy
        monitor.record_message();
        monitor.record_message();
        assert_eq!(
            monitor.poll(&mut Context::from_waker(&noop_waker())),
            Poll::Ready(Some(ConnectionMonitorOutput::Healthy))
        );

        // Recording more than the expected number of messages,
        // expecting the peer to be spammy
        monitor.record_message();
        monitor.record_message();
        monitor.record_message();
        assert_eq!(
            monitor.poll(&mut Context::from_waker(&noop_waker())),
            Poll::Ready(Some(ConnectionMonitorOutput::Spammy))
        );

        // Recording less than the expected number of messages (i.e. no message),
        // expecting the peer to be unhealthy
        assert_eq!(
            monitor.poll(&mut Context::from_waker(&noop_waker())),
            Poll::Ready(Some(ConnectionMonitorOutput::Unhealthy))
        );

        // Recording the right number of messages, marks the peer as healthy again.
        monitor.record_message();
        assert_eq!(
            monitor.poll(&mut Context::from_waker(&noop_waker())),
            Poll::Ready(Some(ConnectionMonitorOutput::Healthy))
        );
    }

    #[tokio::test]
    async fn monitor_interval() {
        let interval = Duration::from_millis(100);
        let mut monitor = ConnectionMonitor::new(
            tokio_stream::wrappers::IntervalStream::new(tokio::time::interval_at(
                tokio::time::Instant::now() + interval,
                interval,
            ))
            .map(|_| 1..=2),
        );

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        tokio::time::sleep(interval).await;

        // First poll is pending because the range has not been set.
        assert!(monitor.poll(&mut cx).is_pending());
        tokio::time::sleep(interval).await;
        assert!(monitor.poll(&mut cx).is_ready());
        assert!(monitor.poll(&mut cx).is_pending());
    }
}
