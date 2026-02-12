use lb_api_service::ApiServiceSettings;
use lb_http_api_common::settings::AxumBackendSettings;

use crate::config::api::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[cfg(not(feature = "testing"))]
    #[must_use]
    pub fn into_backend_settings(self) -> ApiServiceSettings<AxumBackendSettings> {
        ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                address: self.user.backend.listen_address,
                cors_origins: self.user.backend.cors_origins,
                timeout: self.user.backend.timeout,
                max_body_size: self.user.backend.max_body_size as usize,
                max_concurrent_requests: self.user.backend.max_concurrent_requests as usize,
            },
        }
    }

    #[cfg(feature = "testing")]
    #[must_use]
    pub fn into_backend_and_testing_settings(
        self,
    ) -> (
        ApiServiceSettings<AxumBackendSettings>,
        ApiServiceSettings<AxumBackendSettings>,
    ) {
        let backend_settings = AxumBackendSettings {
            address: self.user.backend.listen_address,
            cors_origins: self.user.backend.cors_origins,
            timeout: self.user.backend.timeout,
            max_body_size: self.user.backend.max_body_size as usize,
            max_concurrent_requests: self.user.backend.max_concurrent_requests as usize,
        };

        let testing_settings = AxumBackendSettings {
            address: self.user.testing.listen_address,
            cors_origins: self.user.testing.cors_origins,
            timeout: self.user.testing.timeout,
            max_body_size: self.user.testing.max_body_size as usize,
            max_concurrent_requests: self.user.testing.max_concurrent_requests as usize,
        };

        (
            ApiServiceSettings { backend_settings },
            ApiServiceSettings {
                backend_settings: testing_settings,
            },
        )
    }
}
