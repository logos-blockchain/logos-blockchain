use core::time::Duration;

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::config::deployment::WellKnownDeployment;

#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
}

impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Devnet => devnet_settings(),
        }
    }
}

const fn devnet_settings() -> Settings {
    Settings {
        slot_duration: Duration::from_secs(1),
    }
}
