use crate::config::tracing::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}
