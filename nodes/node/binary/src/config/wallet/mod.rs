use crate::config::wallet::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}
