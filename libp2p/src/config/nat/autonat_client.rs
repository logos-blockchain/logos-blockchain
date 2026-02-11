use std::time::Duration;

use libp2p::autonat::v2::client::Config;
use serde::{Deserialize, Serialize};

/// A serializable representation of `AutoNAT` configuration options.
/// When a value is None, the libp2p defaults are used.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct Settings {
    /// How many candidates we will test at most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_candidates: Option<usize>,

    /// The interval at which we will attempt to confirm candidates as external
    /// addresses, only used for new candidates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_interval_millisecs: Option<u64>,

    /// The interval at which we will retest successful external addresses.
    /// This is used to ensure that the external address is still valid and
    /// reachable.
    #[serde(default = "default_retest_interval")]
    pub retest_successful_external_addresses_interval: Duration,
}

const fn default_retest_interval() -> Duration {
    Duration::from_secs(60)
}

impl Settings {
    #[must_use]
    pub fn to_libp2p_config(&self) -> Config {
        let mut config = Config::default();

        // Apply only the settings that were specified, leaving libp2p defaults for the
        // rest
        if let Some(max_candidates) = self.max_candidates {
            config = config.with_max_candidates(max_candidates);
        }

        if let Some(probe_interval_millisecs) = self.probe_interval_millisecs {
            config = config.with_probe_interval(Duration::from_millis(probe_interval_millisecs));
        }

        config
    }

    #[must_use]
    pub const fn with_probe_interval_millisecs(mut self, millisecs: u64) -> Self {
        self.probe_interval_millisecs = Some(millisecs);
        self
    }
}
