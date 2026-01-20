pub(crate) mod common;
pub mod executor;
pub mod validator;

pub use common::{
    ReplicationConfig,
    balancer::BalancerStats,
    monitor::{DAConnectionMonitorSettings, dto::MonitorStats},
    policy::DAConnectionPolicySettings,
};
use futures::channel::oneshot;

use crate::protocols::dispersal::validator::behaviour::DispersalEvent;

pub(crate) type ConnectionMonitor<Membership> =
    common::monitor::DAConnectionMonitor<common::policy::DAConnectionPolicy<Membership>>;

pub(crate) type ConnectionBalancer<Membership> = common::balancer::DAConnectionBalancer<
    Membership,
    common::policy::DAConnectionPolicy<Membership>,
>;

/// Dispersed data failed to be validated.
#[derive(Debug)]
pub struct DispersalValidationError;

pub type DispersalValidationResult = Result<(), DispersalValidationError>;

pub type ValidationResultSender = Option<oneshot::Sender<DispersalValidationResult>>;

pub struct DispersalValidatorEvent {
    pub event: DispersalEvent,
    pub sender: ValidationResultSender,
}
