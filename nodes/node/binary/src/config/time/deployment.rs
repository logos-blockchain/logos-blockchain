use core::time::Duration;

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use time::OffsetDateTime;

#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
    pub chain_start_time: OffsetDateTime,
}
