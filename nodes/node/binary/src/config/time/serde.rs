use lb_time_service::backends::NtpTimeBackendSettings;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub backend: NtpTimeBackendSettings,
}
