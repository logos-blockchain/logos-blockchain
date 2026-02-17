#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use core::time::Duration;

use lb_utils::{
    bounded_duration::{MinimalBoundedDuration, SECOND},
    math::PositiveF64,
};
use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::config::utils;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct TraversalConfig {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub autonat: AutonatClientConfig,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub mapping: MappingConfig,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub gateway_monitor: GatewayConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AutonatClientConfig {
    /// How many candidates we will test at most.
    #[serde(skip_serializing_if = "utils::is_default")]
    pub max_candidates: Option<usize>,

    /// The interval at which we will attempt to confirm candidates as external
    /// addresses, only used for new candidates.
    #[serde(skip_serializing_if = "utils::is_default")]
    pub probe_interval_millisecs: Option<u64>,

    /// The interval at which we will retest successful external addresses.
    /// This is used to ensure that the external address is still valid and
    /// reachable.
    #[serde(skip_serializing_if = "is_default_retest_successful_external_addresses_interval")]
    pub retest_successful_external_addresses_interval: Duration,
}

const fn default_retest_successful_external_addresses_interval() -> Duration {
    Duration::from_secs(60)
}

fn is_default_retest_successful_external_addresses_interval(interval: &Duration) -> bool {
    *interval == default_retest_successful_external_addresses_interval()
}

impl Default for AutonatClientConfig {
    fn default() -> Self {
        Self {
            max_candidates: None,
            probe_interval_millisecs: None,
            retest_successful_external_addresses_interval:
                default_retest_successful_external_addresses_interval(),
        }
    }
}

#[serde_as]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MappingConfig {
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    #[serde(skip_serializing_if = "is_default_timeout")]
    pub timeout: Duration,
    #[serde(skip_serializing_if = "is_default_lease_duration")]
    pub lease_duration: Duration,
    #[serde(skip_serializing_if = "is_default_max_retries")]
    pub max_retries: u32,
    #[serde(skip_serializing_if = "is_default_renewal_delay_fraction")]
    pub renewal_delay_fraction: PositiveF64,
    #[serde(skip_serializing_if = "is_default_retry_interval")]
    pub retry_interval: Duration,
}

const fn default_timeout() -> Duration {
    Duration::from_secs(1)
}

fn is_default_timeout(timeout: &Duration) -> bool {
    *timeout == default_timeout()
}

const fn default_lease_duration() -> Duration {
    Duration::from_hours(2)
}

fn is_default_lease_duration(lease_duration: &Duration) -> bool {
    *lease_duration == default_lease_duration()
}

const fn default_max_retries() -> u32 {
    3
}

const fn is_default_max_retries(max_retries: &u32) -> bool {
    *max_retries == default_max_retries()
}

fn default_renewal_delay_fraction() -> PositiveF64 {
    PositiveF64::try_from(0.8).expect("0.8 is positive")
}

fn is_default_renewal_delay_fraction(renewal_delay_fraction: &PositiveF64) -> bool {
    *renewal_delay_fraction == default_renewal_delay_fraction()
}

const fn default_retry_interval() -> Duration {
    Duration::from_secs(30)
}

fn is_default_retry_interval(retry_interval: &Duration) -> bool {
    *retry_interval == default_retry_interval()
}

impl Default for MappingConfig {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            lease_duration: default_lease_duration(),
            max_retries: default_max_retries(),
            renewal_delay_fraction: default_renewal_delay_fraction(),
            retry_interval: default_retry_interval(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GatewayConfig {
    /// How often to check for gateway address changes
    #[serde(skip_serializing_if = "is_default_check_interval")]
    pub check_interval: Duration,
}

const fn default_check_interval() -> Duration {
    Duration::from_mins(5)
}

fn is_default_check_interval(check_interval: &Duration) -> bool {
    *check_interval == default_check_interval()
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            check_interval: default_check_interval(),
        }
    }
}
