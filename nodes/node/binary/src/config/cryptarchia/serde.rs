use core::{num::NonZeroUsize, time::Duration};
use std::{collections::HashSet, path::PathBuf};

use lb_core::mantle::Value;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_libp2p::PeerId;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub service: ServiceConfig,
    pub network: NetworkConfig,
    pub leader: LeaderConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub recovery_file: PathBuf,
    pub bootstrap: BootstrapConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde_as]
pub struct BootstrapConfig {
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub prolonged_bootstrap_period: Duration,
    pub force_bootstrap: bool,
    #[serde(default)]
    pub offline_grace_period: OfflineGracePeriodConfig,
}

impl From<BootstrapConfig> for lb_chain_service::BootstrapConfig {
    fn from(config: BootstrapConfig) -> Self {
        Self {
            prolonged_bootstrap_period: config.prolonged_bootstrap_period,
            force_bootstrap: config.force_bootstrap,
            offline_grace_period: config.offline_grace_period.into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde_as]
pub struct OfflineGracePeriodConfig {
    /// Maximum duration a node can be offline before forcing bootstrap mode
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    #[serde(default = "default_offline_grace_period")]
    pub grace_period: Duration,
    /// Interval at which to record the current timestamp and engine state
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    #[serde(default = "default_state_recording_interval")]
    pub state_recording_interval: Duration,
}

impl Default for OfflineGracePeriodConfig {
    fn default() -> Self {
        Self {
            grace_period: default_offline_grace_period(),
            state_recording_interval: default_state_recording_interval(),
        }
    }
}

impl From<OfflineGracePeriodConfig> for lb_chain_service::OfflineGracePeriodConfig {
    fn from(config: OfflineGracePeriodConfig) -> Self {
        Self {
            grace_period: config.grace_period,
            state_recording_interval: config.state_recording_interval,
        }
    }
}

const fn default_offline_grace_period() -> Duration {
    Duration::from_secs(20 * 60)
}

const fn default_state_recording_interval() -> Duration {
    Duration::from_secs(60)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub bootstrap: NetworkBootstrapConfig,
    pub sync: SyncConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde_as]
pub struct NetworkBootstrapConfig {
    pub ibd: IbdConfig,
}

impl From<NetworkBootstrapConfig> for lb_chain_network_service::BootstrapConfig<PeerId> {
    fn from(config: NetworkBootstrapConfig) -> Self {
        Self {
            ibd: config.ibd.into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IbdConfig {
    /// Peers to download blocks from.
    pub peers: HashSet<PeerId>,
    /// Delay before attempting the next download
    /// when no download is needed at the moment from a peer.
    #[serde(default = "default_delay_before_new_download")]
    pub delay_before_new_download: Duration,
}

impl From<IbdConfig> for lb_chain_network_service::IbdConfig<PeerId> {
    fn from(config: IbdConfig) -> Self {
        Self {
            peers: config.peers,
            delay_before_new_download: config.delay_before_new_download,
        }
    }
}

const fn default_delay_before_new_download() -> Duration {
    Duration::from_secs(10)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncConfig {
    pub orphan: OrphanConfig,
}

impl From<SyncConfig> for lb_chain_network_service::SyncConfig {
    fn from(config: SyncConfig) -> Self {
        Self {
            orphan: config.orphan.into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrphanConfig {
    /// The maximum number of pending orphans to keep in the cache.
    #[serde(default = "default_max_orphan_cache_size")]
    pub max_orphan_cache_size: NonZeroUsize,
}

impl From<OrphanConfig> for lb_chain_network_service::OrphanConfig {
    fn from(config: OrphanConfig) -> Self {
        Self {
            max_orphan_cache_size: config.max_orphan_cache_size,
        }
    }
}

const fn default_max_orphan_cache_size() -> NonZeroUsize {
    NonZeroUsize::new(5).unwrap()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaderConfig {
    pub wallet: LeaderWalletConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaderWalletConfig {
    // Hard cap on the transaction fee for LEADER_CLAIM
    pub max_tx_fee: Value,

    // The key to use for paying transaction fees for LEADER_CLAIM.
    // Change notes will be returned to this same funding pk.
    pub funding_pk: ZkPublicKey,
}

impl From<LeaderWalletConfig> for lb_chain_leader_service::LeaderWalletConfig {
    fn from(config: LeaderWalletConfig) -> Self {
        Self {
            max_tx_fee: config.max_tx_fee,
            funding_pk: config.funding_pk,
        }
    }
}
