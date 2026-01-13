use core::time::Duration;

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::config::deployment::WellKnownDeployment;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde_as]
pub struct Settings {
    #[serde_as(as = "nomos_utils::bounded_duration::MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
}

impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Mainnet => Self {
                slot_duration: Duration::from_secs(1),
            },
        }
    }
}
