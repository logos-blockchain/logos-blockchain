//! Defines the [`ServiceStatusEntry`] type, which associates a service
//! identifier with its current [`ServiceStatus`].
//!
//! This module is useful for tracking the readiness or operational state of
//! services within an Overwatch-based system. It provides an ergonomic way to
//! collect and display service statuses, particularly when aggregating
//! diagnostics or errors.

use std::fmt::{Debug, Display, Formatter};

use overwatch::services::status::ServiceStatus;

pub struct ServiceStatusEntry<RuntimeServiceId> {
    id: RuntimeServiceId,
    status: ServiceStatus,
}

impl<RuntimeServiceId> ServiceStatusEntry<RuntimeServiceId> {
    pub const fn from_overwatch(id: RuntimeServiceId, status: ServiceStatus) -> Self {
        Self { id, status }
    }
}

impl<RuntimeServiceId: Display> Display for ServiceStatusEntry<RuntimeServiceId> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let Self { id, status } = self;
        let id = id.to_string();
        write!(f, "{id}: {status}")
    }
}

impl<RuntimeServiceId: Display> Debug for ServiceStatusEntry<RuntimeServiceId> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let Self { id, status } = self;
        let id = id.to_string();
        write!(f, "ServiceStatusEntry {{ id: {id}, status: {status:?} }}")
    }
}

impl<RuntimeServiceId: Display> std::error::Error for ServiceStatusEntry<RuntimeServiceId> {}
