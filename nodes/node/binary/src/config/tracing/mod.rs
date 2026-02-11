use lb_tracing_service::TracingSettings;

use crate::config::tracing::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for TracingSettings {
    fn from(value: ServiceConfig) -> Self {
        TracingSettings {
            logger: value.user.logger,
            tracing: value.user.tracing,
            filter: value.user.filter,
            metrics: value.user.metrics,
            console: value.user.console,
            level: value.user.level,
        }
    }
}
