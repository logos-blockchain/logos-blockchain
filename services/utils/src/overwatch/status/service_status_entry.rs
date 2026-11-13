//! A service identifier and its observed status.

use std::fmt::{Display, Formatter};

use overwatch::services::status::ServiceStatus;

#[derive(Debug, PartialEq, Eq)]
pub struct ServiceStatusEntry<RuntimeServiceId> {
    pub id: RuntimeServiceId,
    pub status: ServiceStatus,
}

impl<RuntimeServiceId> ServiceStatusEntry<RuntimeServiceId> {
    pub const fn from_overwatch(id: RuntimeServiceId, status: ServiceStatus) -> Self {
        Self { id, status }
    }
}

impl<RuntimeServiceId: Display> Display for ServiceStatusEntry<RuntimeServiceId> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.id, self.status)
    }
}
