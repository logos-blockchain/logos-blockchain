use core::time::Duration;

use lb_utils::{
    bounded_duration::{MILLI, MinimalBoundedDuration, SECOND},
    math::PositiveF64,
};
use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Config {
    /// NAT traversal with autonat, mapping, and gateway monitoring
    Traversal(TraversalConfig),
    /// Static external address for nodes with fixed public IPs
    Static {
        /// The fixed external address to use (NAT traversal disabled)
        external_address: Multiaddr,
    },
}

impl From<Config> for lb_libp2p::NatSettings {
    fn from(config: Config) -> Self {
        match config {
            Config::Traversal(traversal_config) => Self::Traversal(lb_libp2p::TraversalSettings {
                autonat: lb_libp2p::AutonatClientSettings {
                    max_candidates: traversal_config.autonat.max_candidates,
                    probe_interval: traversal_config.autonat.probe_interval,
                    retest_successful_external_addresses_interval: traversal_config
                        .autonat
                        .retest_successful_external_addresses_interval,
                },
                mapping: lb_libp2p::NatMappingSettings {
                    timeout: traversal_config.mapping.timeout,
                    lease_duration: traversal_config.mapping.lease_duration,
                    max_retries: traversal_config.mapping.max_retries,
                    renewal_delay_fraction: traversal_config.mapping.renewal_delay_fraction,
                    retry_interval: traversal_config.mapping.retry_interval,
                },
                gateway_monitor: lb_libp2p::GatewaySettings {
                    check_interval: traversal_config.gateway_monitor.check_interval,
                },
            }),
            Config::Static { external_address } => Self::Static { external_address },
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::Traversal(TraversalConfig::default())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TraversalConfig {
    pub autonat: AutonatClientConfig,
    pub mapping: MappingConfig,
    pub gateway_monitor: GatewayConfig,
}

#[serde_as]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonatClientConfig {
    /// How many candidates we will test at most.
    pub max_candidates: Option<usize>,

    /// The interval at which we will attempt to confirm candidates as external
    /// addresses, only used for new candidates.
    #[serde_as(as = "Option<MinimalBoundedDuration<1, MILLI>>")]
    pub probe_interval: Option<Duration>,

    /// The interval at which we will retest successful external addresses.
    /// This is used to ensure that the external address is still valid and
    /// reachable.
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub retest_successful_external_addresses_interval: Duration,
}

impl Default for AutonatClientConfig {
    fn default() -> Self {
        Self {
            max_candidates: None,
            probe_interval: None,
            retest_successful_external_addresses_interval: Duration::from_mins(1),
        }
    }
}

#[serde_as]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct MappingConfig {
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub timeout: Duration,
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub lease_duration: Duration,
    pub max_retries: u32,
    pub renewal_delay_fraction: PositiveF64,
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub retry_interval: Duration,
}

impl Default for MappingConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(1),
            lease_duration: Duration::from_hours(2),
            max_retries: 3,
            renewal_delay_fraction: PositiveF64::try_from(0.8).expect("0.8 is positive"),
            retry_interval: Duration::from_secs(30),
        }
    }
}

#[serde_as]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct GatewayConfig {
    /// How often to check for gateway address changes
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub check_interval: Duration,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_mins(5),
        }
    }
}
