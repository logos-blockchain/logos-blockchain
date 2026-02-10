use crate::config::kms::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}
