use core::{num::NonZero, time::Duration};

use lb_binary_codec::canonical::BinaryEncode;
use serde::{Deserialize, Serialize};

pub(crate) mod codec;

#[derive(Serialize, Deserialize, Debug, Clone, BinaryEncode)]
pub struct Settings {
    /// How long a pending transaction may stay in the mempool before it is
    /// retired.
    pub transaction_ttl_in_seconds: NonZero<u64>,
    /// How long a retired transaction stays resolvable after its age reaches
    /// the TTL.
    pub retirement_grace_in_seconds: NonZero<u64>,
}

impl Settings {
    #[must_use]
    pub const fn transaction_ttl(&self) -> Duration {
        Duration::from_secs(self.transaction_ttl_in_seconds.get())
    }

    #[must_use]
    pub const fn retirement_grace(&self) -> Duration {
        Duration::from_secs(self.retirement_grace_in_seconds.get())
    }
}
