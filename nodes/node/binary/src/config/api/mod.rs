use lb_api_service::ApiServiceSettings;
use lb_http_api_common::settings::AxumBackendSettings;

use crate::config::api::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn backend_settings(&self) -> ApiServiceSettings<AxumBackendSettings> {
        ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                address: self.user.backend.listen_address,
                cors_origins: self.user.backend.cors_origins.clone(),
                timeout: self.user.backend.timeout,
                max_body_size: self.user.backend.max_body_size as usize,
                max_concurrent_requests: self.user.backend.max_concurrent_requests as usize,
            },
        }
    }

    #[cfg(feature = "testing")]
    #[must_use]
    pub fn testing_settings(&self) -> ApiServiceSettings<AxumBackendSettings> {
        ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                address: self.user.testing.listen_address,
                cors_origins: self.user.testing.cors_origins.clone(),
                timeout: self.user.testing.timeout,
                max_body_size: self.user.testing.max_body_size as usize,
                max_concurrent_requests: self.user.testing.max_concurrent_requests as usize,
            },
        }
    }
}
