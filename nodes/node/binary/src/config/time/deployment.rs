use core::time::Duration;

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde_as]
pub struct Settings {
    #[serde_as(as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
}
