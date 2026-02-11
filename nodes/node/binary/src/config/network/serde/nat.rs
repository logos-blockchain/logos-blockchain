use core::time::Duration;

use lb_utils::{
    bounded_duration::{MinimalBoundedDuration, SECOND},
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
                    probe_interval_millisecs: traversal_config.autonat.probe_interval_millisecs,
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
pub struct TraversalConfig {
    pub autonat: AutonatClientConfig,
    pub mapping: MappingConfig,
    pub gateway_monitor: GatewayConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct AutonatClientConfig {
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

#[serde_as]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MappingConfig {
    #[serde(default = "default_timeout")]
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub timeout: Duration,
    #[serde(default = "default_lifetime")]
    pub lease_duration: Duration,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_renewal_delay_fraction")]
    pub renewal_delay_fraction: PositiveF64,
    #[serde(default = "default_retry_interval")]
    pub retry_interval: Duration,
}

const fn default_timeout() -> Duration {
    Duration::from_secs(1)
}

const fn default_lifetime() -> Duration {
    Duration::from_secs(7200) // 2 hours
}

const fn default_max_retries() -> u32 {
    3
}

fn default_renewal_delay_fraction() -> PositiveF64 {
    PositiveF64::try_from(0.8).expect("0.8 is positive")
}

const fn default_retry_interval() -> Duration {
    Duration::from_secs(30)
}

impl Default for MappingConfig {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            lease_duration: default_lifetime(),
            max_retries: default_max_retries(),
            renewal_delay_fraction: default_renewal_delay_fraction(),
            retry_interval: default_retry_interval(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// How often to check for gateway address changes
    #[serde(default = "default_check_interval")]
    pub check_interval: Duration,
}

const fn default_check_interval() -> Duration {
    Duration::from_secs(300)
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            check_interval: default_check_interval(),
        }
    }
}
