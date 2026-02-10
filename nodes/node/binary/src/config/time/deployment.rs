use core::time::Duration;

use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use time::OffsetDateTime;

use crate::config::deployment::WellKnownDeployment;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde_as]
pub struct Settings {
    #[serde_as(as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
    pub chain_start_time: OffsetDateTime,
}

impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Devnet => devnet_settings(),
        }
    }
}

fn devnet_settings() -> Settings {
    Settings {
        slot_duration: Duration::from_secs(1),
        // TODO: Change to real value once we have a stable devnet value.
        chain_start_time: OffsetDateTime::now_utc(),
    }
}
