use lb_tracing_service::TracingSettings;

use crate::config::tracing::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for TracingSettings {
    fn from(value: ServiceConfig) -> Self {
        Self {
            logger: value.user.logger.into(),
            tracing: value.user.tracing.into(),
            filter: value.user.filter.into(),
            metrics: value.user.metrics.into(),
            console: value.user.console.into(),
            level: value.user.level,
        }
    }
}
