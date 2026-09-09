use lb_api_service::ApiServiceSettings;
use lb_core::mantle::transactions::genesis_tx::ChainId;
use lb_http_api_common::settings::AxumBackendSettings;

use crate::config::api::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
    /// The chain this node runs on, taken from the deployment settings.
    pub chain_id: ChainId,
}

impl ServiceConfig {
    #[must_use]
    pub fn backend_settings(&self) -> ApiServiceSettings<AxumBackendSettings> {
        ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                chain_id: self.chain_id.clone(),
                address: self.user.backend.listen_address,
                cors_origins: self.user.backend.cors_origins.clone(),
                timeout: self.user.backend.timeout,
                max_body_size: self.user.backend.max_body_size as usize,
                max_concurrent_requests: self.user.backend.max_concurrent_requests as usize,
            },
        }
    }
}
