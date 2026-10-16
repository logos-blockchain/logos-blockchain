use core::{num::NonZero, time::Duration};

use serde::{Deserialize, Serialize};

pub(crate) mod codec;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub slot_duration_in_seconds: NonZero<u64>,
}

impl Settings {
    #[must_use]
    pub const fn slot_duration(&self) -> Duration {
        Duration::from_secs(self.slot_duration_in_seconds.get())
    }
}
