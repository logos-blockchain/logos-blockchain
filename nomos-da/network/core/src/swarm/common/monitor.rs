use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use fixed::types::U57F7;
use libp2p::PeerId;
use nomos_utils::bounded_duration::{MinimalBoundedDuration, NANO};
use serde::{Deserialize, Serialize};

use crate::{
    maintenance::monitor::{ConnectionMonitor, ConnectionMonitorOutput, PeerStatus},
    protocols::{
        dispersal::{
            executor::behaviour::{
                DispersalError as ExecutorDispersalError, DispersalExecutorEvent,
            },
            validator::behaviour::{
                DispersalError as ValidatorDispersalError,
                DispersalEvent as DispersalValidatorEvent,
            },
        },
        replication::behaviour::{ReplicationError, ReplicationEvent},
        sampling::{SamplingEvent, errors::SamplingError},
    },
};

#[derive(Debug)]
pub enum MonitorEvent {
    ExecutorDispersal(ExecutorDispersalError),
    ValidatorDispersal(ValidatorDispersalError),
    Replication(ReplicationError),
    Sampling(SamplingError),
    Noop,
}

impl MonitorEvent {
    pub const fn peer_id(&self) -> Option<&PeerId> {
        match self {
            Self::ExecutorDispersal(dispersal_error) => dispersal_error.peer_id(),
            Self::ValidatorDispersal(dispersal_error) => dispersal_error.peer_id(),
            Self::Replication(replication_error) => replication_error.peer_id(),
            Self::Sampling(sampling_error) => sampling_error.peer_id(),
            Self::Noop => None,
        }
    }
}

impl From<&DispersalExecutorEvent> for MonitorEvent {
    fn from(event: &DispersalExecutorEvent) -> Self {
        match event {
            DispersalExecutorEvent::DispersalSuccess { .. } => Self::Noop,
            DispersalExecutorEvent::DispersalError { error } => match error {
                // Only map Io or OpenStreamError to MonitorEvent
                &ExecutorDispersalError::Io { .. }
                | &ExecutorDispersalError::OpenStreamError { .. } => {
                    Self::ExecutorDispersal(error.clone())
                }
                _ => Self::Noop, // All other cases return Noop
            },
        }
    }
}

impl From<&DispersalValidatorEvent> for MonitorEvent {
    fn from(event: &DispersalValidatorEvent) -> Self {
        match event {
            DispersalValidatorEvent::IncomingShare { .. }
            | DispersalValidatorEvent::IncomingTx { .. } => Self::Noop,
            DispersalValidatorEvent::DispersalError { error } => {
                Self::ValidatorDispersal(error.clone())
            }
        }
    }
}

impl From<&ReplicationEvent> for MonitorEvent {
    fn from(event: &ReplicationEvent) -> Self {
        match event {
            ReplicationEvent::IncomingMessage { .. } => Self::Noop,
            ReplicationEvent::ReplicationError { error } => Self::Replication(error.clone()),
        }
    }
}

impl From<&SamplingEvent> for MonitorEvent {
    fn from(event: &SamplingEvent) -> Self {
        match event {
            SamplingEvent::SamplingError { error } => match error {
                // Only map Io or OpenStreamError to Self
                &SamplingError::Io { .. } | &SamplingError::OpenStream { .. } => {
                    Self::Sampling(error.clone())
                }
                _ => Self::Noop, // All other cases return Noop
            },
            SamplingEvent::SamplingSuccess { .. }
            | SamplingEvent::IncomingSample { .. }
            | SamplingEvent::CommitmentsSuccess { .. }
            | SamplingEvent::HistoricSamplingSuccess { .. }
            | SamplingEvent::HistoricSamplingError { .. }
            | SamplingEvent::HistoricCommitmentsSuccess { .. }
            | SamplingEvent::HistoricCommitmentsError { .. }
            | SamplingEvent::Opinion(_) => Self::Noop,
        }
    }
}

/// Data Transfer Object (DTO) for the connection monitor stats.
pub mod dto {
    use std::collections::HashMap;

    use fixed::types::U57F7;
    use libp2p::PeerId;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    pub struct MonitorStats(pub HashMap<PeerId, PeerStats>);

    impl From<&HashMap<PeerId, super::PeerStats>> for MonitorStats {
        fn from(stats: &HashMap<PeerId, super::PeerStats>) -> Self {
            Self(stats.iter().map(|(k, v)| (*k, v.into())).collect())
        }
    }

    #[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    pub struct PeerStats {
        // Workaround for clippy: "all fields have the same postfix: `failures_rate`"
        #[serde(rename = "dispersal_failure_rate")]
        pub dispersal: U57F7,
        #[serde(rename = "sampling_failure_rate")]
        pub sampling: U57F7,
        #[serde(rename = "replication_failure_rate")]
        pub replication: U57F7,
    }

    impl From<&super::PeerStats> for PeerStats {
        fn from(stats: &super::PeerStats) -> Self {
            Self {
                dispersal: stats.dispersal_failures_rate,
                sampling: stats.sampling_failures_rate,
                replication: stats.replication_failures_rate,
            }
        }
    }
}

/// Tracks failure rates for different protocols using exponential weighted
/// moving average.
#[derive(Copy, Clone, Default, Debug)]
pub struct PeerStats {
    // Calculated using EWMA to give more weight to recent failures while gradually decaying over
    // time.
    pub dispersal_failures_rate: U57F7,
    pub sampling_failures_rate: U57F7,
    pub replication_failures_rate: U57F7,

    // Track the time of the last failure for decay calculations.
    last_dispersal_failure: Option<Instant>,
    last_sampling_failure: Option<Instant>,
    last_replication_failure: Option<Instant>,
}

impl PeerStats {
    pub fn compute_dispersal_failure_rate(
        &self,
        now: Instant,
        window: Duration,
        factor: U57F7,
    ) -> U57F7 {
        compute_failure_rate(
            now,
            self.last_dispersal_failure.unwrap_or(now),
            self.dispersal_failures_rate,
            window,
            factor,
        )
    }

    pub fn compute_sampling_failure_rate(
        &self,
        now: Instant,
        window: Duration,
        factor: U57F7,
    ) -> U57F7 {
        compute_failure_rate(
            now,
            self.last_sampling_failure.unwrap_or(now),
            self.sampling_failures_rate,
            window,
            factor,
        )
    }

    /// **Updates the replication failure rate**
    pub fn compute_replication_failure_rate(
        &self,
        now: Instant,
        window: Duration,
        factor: U57F7,
    ) -> U57F7 {
        compute_failure_rate(
            now,
            self.last_replication_failure.unwrap_or(now),
            self.replication_failures_rate,
            window,
            factor,
        )
    }

    pub fn get_updated_stats(
        &self,
        now: Instant,
        time_window: Duration,
        decay_factor: U57F7,
    ) -> Self {
        Self {
            dispersal_failures_rate: self.compute_dispersal_failure_rate(
                now,
                time_window,
                decay_factor,
            ),
            sampling_failures_rate: self.compute_sampling_failure_rate(
                now,
                time_window,
                decay_factor,
            ),
            replication_failures_rate: self.compute_replication_failure_rate(
                now,
                time_window,
                decay_factor,
            ),
            last_dispersal_failure: self.last_dispersal_failure,
            last_sampling_failure: self.last_sampling_failure,
            last_replication_failure: self.last_replication_failure,
        }
    }
}

pub trait PeerHealthPolicy {
    type PeerStats;

    /// Evaluates whether a peer is malicious.
    ///
    /// Returns `true` if the peer is deemed malicious, otherwise `false`.
    fn is_peer_malicious(&self, stats: &Self::PeerStats) -> bool;

    /// Evaluates whether a peer is unhealthy.
    ///
    /// Returns `true` if the peer is deemed unhealthy, otherwise `false`.
    fn is_peer_unhealthy(&self, stats: &Self::PeerStats) -> bool;
}

#[serde_with::serde_as]
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DAConnectionMonitorSettings {
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub failure_time_window: Duration,
    pub time_decay_factor: U57F7,
}

pub struct DAConnectionMonitor<Policy> {
    peer_stats: HashMap<PeerId, PeerStats>,
    policy: Policy,
    settings: DAConnectionMonitorSettings,
}

impl<Policy> DAConnectionMonitor<Policy>
where
    Policy: PeerHealthPolicy<PeerStats = PeerStats>,
{
    pub fn new(settings: DAConnectionMonitorSettings, policy: Policy) -> Self {
        Self {
            peer_stats: HashMap::new(),
            policy,
            settings,
        }
    }

    fn evaluate_peer(&self, now: Instant, peer_id: &PeerId) -> PeerStatus {
        if let Some(stats) = self.peer_stats.get(peer_id) {
            // We need to recompute the failure rate upon the evaluation as time has moved
            // on and the failure rate must have decayed.
            let stats = stats.get_updated_stats(
                now,
                self.settings.failure_time_window,
                self.settings.time_decay_factor,
            );

            if self.policy.is_peer_malicious(&stats) {
                return PeerStatus::Malicious;
            }

            if self.policy.is_peer_unhealthy(&stats) {
                return PeerStatus::Unhealthy;
            }
        }
        PeerStatus::Healthy
    }
}

impl<Policy> ConnectionMonitor for DAConnectionMonitor<Policy>
where
    Policy: PeerHealthPolicy<PeerStats = PeerStats>,
{
    type Event = MonitorEvent;
    type Stats = dto::MonitorStats;

    fn record_event(&mut self, event: Self::Event) -> Option<ConnectionMonitorOutput> {
        if let Some(peer_id) = event.peer_id() {
            tracing::info!("MONITOR EVENT: {event:?}");
            let stats = self.peer_stats.entry(*peer_id).or_default();
            let now = Instant::now();
            match event {
                MonitorEvent::ExecutorDispersal(_) | MonitorEvent::ValidatorDispersal(_) => {
                    stats.dispersal_failures_rate = stats.compute_dispersal_failure_rate(
                        now,
                        self.settings.failure_time_window,
                        self.settings.time_decay_factor,
                    ) + U57F7::ONE; // Compute updated rate and increment by one because its a new error.
                    stats.last_dispersal_failure = Some(now);
                }
                MonitorEvent::Replication(_) => {
                    stats.replication_failures_rate = stats.compute_replication_failure_rate(
                        now,
                        self.settings.failure_time_window,
                        self.settings.time_decay_factor,
                    ) + U57F7::ONE; // Compute updated rate and increment by one because its a new error.
                    stats.last_replication_failure = Some(now);
                }
                MonitorEvent::Sampling(_) => {
                    stats.sampling_failures_rate = stats.compute_sampling_failure_rate(
                        now,
                        self.settings.failure_time_window,
                        self.settings.time_decay_factor,
                    ) + U57F7::ONE; // Compute updated rate and increment by one because its a new error.
                    stats.last_sampling_failure = Some(now);
                }
                MonitorEvent::Noop => {}
            }

            Some(ConnectionMonitorOutput {
                peer_id: *peer_id,
                peer_status: self.evaluate_peer(now, peer_id),
            })
        } else {
            None
        }
    }

    fn reset_peer(&mut self, peer_id: &PeerId) {
        self.peer_stats.remove(peer_id);
    }

    fn stats(&self) -> Self::Stats {
        (&self.peer_stats).into()
    }
}

fn compute_failure_rate(
    now: Instant,
    last_failure: Instant,
    failure_rate: U57F7,
    failure_time_window: Duration,
    decay_factor: U57F7,
) -> U57F7 {
    let elapsed = now.duration_since(last_failure).as_secs_f64();
    let mut new_failure_rate = failure_rate;

    // Apply exponential decay to the failure rate
    if elapsed > 0.0 {
        let time_based_decay = (-elapsed / failure_time_window.as_secs_f64()).exp();
        let decay_factor_fixed = U57F7::from_num(time_based_decay);
        new_failure_rate *= decay_factor_fixed * decay_factor;
    }

    new_failure_rate
}

#[cfg(test)]
mod tests {
    use subnetworks_assignations::versions::v1::FillFromNodeList;

    use super::*;
    use crate::swarm::{DAConnectionPolicySettings, common::policy::DAConnectionPolicy};

    fn setup_monitor(peer_id: PeerId) -> DAConnectionMonitor<DAConnectionPolicy<FillFromNodeList>> {
        let monitor_settings = DAConnectionMonitorSettings {
            failure_time_window: Duration::from_secs(10),
            time_decay_factor: U57F7::lit("0.8"),
        };
        let policy_settings = DAConnectionPolicySettings {
            max_dispersal_failures: 2,
            max_sampling_failures: 2,
            max_replication_failures: 2,
            malicious_threshold: 3,
            min_dispersal_peers: 0,
            min_replication_peers: 0,
        };
        DAConnectionMonitor::new(
            monitor_settings,
            DAConnectionPolicy::new(
                policy_settings,
                FillFromNodeList::new(0, &[PeerId::random()], 0, 0),
                peer_id,
            ),
        )
    }

    #[test]
    fn test_peer_starts_healthy() {
        let peer_id = PeerId::random();
        let monitor = setup_monitor(peer_id);

        assert_eq!(
            monitor.evaluate_peer(Instant::now(), &peer_id),
            PeerStatus::Healthy
        );
    }

    #[test]
    fn test_peer_becomes_unhealthy() {
        let peer_id = PeerId::random();
        let mut monitor = setup_monitor(peer_id);

        for _ in 0..4 {
            monitor.record_event(MonitorEvent::Sampling(SamplingError::Io {
                peer_id,
                error: std::io::Error::other("Simulated I/O error"),
                message: None,
            }));
        }

        assert_eq!(
            monitor.evaluate_peer(Instant::now(), &peer_id),
            PeerStatus::Unhealthy
        );
    }

    #[test]
    fn test_peer_becomes_malicious() {
        let peer_id = PeerId::random();
        let mut monitor = setup_monitor(peer_id);

        for _ in 0..100 {
            monitor.record_event(MonitorEvent::Sampling(SamplingError::Io {
                peer_id,
                error: std::io::Error::other("Simulated I/O error"),
                message: None,
            }));
        }

        assert_eq!(
            monitor.evaluate_peer(Instant::now(), &peer_id),
            PeerStatus::Malicious
        );
    }

    #[test]
    fn test_failure_decay_over_time() {
        let peer_id = PeerId::random();
        let mut monitor = setup_monitor(peer_id);

        for _ in 0..4 {
            monitor.record_event(MonitorEvent::Sampling(SamplingError::Io {
                peer_id,
                error: std::io::Error::other("Simulated I/O error"),
                message: None,
            }));
        }

        assert_eq!(
            monitor.evaluate_peer(Instant::now(), &peer_id),
            PeerStatus::Unhealthy
        );

        // Simulate evaluation after 10 seconds waiting, failure rate should decay,
        // making the peer Healthy again.
        let later = Instant::now() + Duration::from_secs(10);
        assert_eq!(monitor.evaluate_peer(later, &peer_id), PeerStatus::Healthy);
    }

    #[test]
    fn test_peer_reset() {
        let peer_id = PeerId::random();
        let mut monitor = setup_monitor(peer_id);

        for _ in 0..4 {
            monitor.record_event(MonitorEvent::Sampling(SamplingError::Io {
                peer_id,
                error: std::io::Error::other("Simulated I/O error"),
                message: None,
            }));
        }

        assert_eq!(
            monitor.evaluate_peer(Instant::now(), &peer_id),
            PeerStatus::Unhealthy
        );

        monitor.reset_peer(&peer_id);

        assert_eq!(
            monitor.evaluate_peer(Instant::now(), &peer_id),
            PeerStatus::Healthy
        );
    }
}
