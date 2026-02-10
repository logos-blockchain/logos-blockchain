use crate::config::storage::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}
