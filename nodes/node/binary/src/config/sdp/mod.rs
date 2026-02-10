use crate::config::sdp::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}
