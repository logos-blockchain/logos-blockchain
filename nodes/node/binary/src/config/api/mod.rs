use crate::config::api::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}
