use nomos_time::backends::NtpTimeBackendSettings;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub backend: NtpTimeBackendSettings,
    pub chain_start_time: OffsetDateTime,
}
