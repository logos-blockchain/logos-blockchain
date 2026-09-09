use std::{
    collections::{HashMap, HashSet},
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    num::NonZero,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use cucumber::gherkin::Table;
use futures::future::try_join_all;
use hex::ToHex as _;
use lb_chain_service::{ChainServiceInfo, CryptarchiaInfo, PhaseTag};
use lb_config::kms::key_id_for_preload_backend;
use lb_core::mantle::{Utxo, ops::OpId as _};
use lb_http_api_common::paths::CRYPTARCHIA_INFO;
use lb_key_management_system_service::{backend::preload::KeyId, keys::Key};
use lb_libp2p::PeerId;
use lb_node::config::{
    DeploymentSettings, RunConfig,
    tracing::serde::console::{Layer as ConsoleLayer, TokioConfig},
};
use lb_testing_framework::{
    LbcEnv, LbcManualCluster, NodeHttpClient, USER_CONFIG_FILE, configs::wallet::WalletAccount,
};
use libp2p::Multiaddr;
use reqwest::{Client, Url};
use testing_framework_core::scenario::{PeerSelection, StartNodeOptions, StartedNode};
use tokio::time::{Instant as TokioInstant, sleep, timeout};
use tracing::{info, warn};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::{
        TARGET,
        nodes::{
            config_override::{apply_deployment_config_overrides, apply_user_config_overrides},
            diagnostics::log_node_lifecycle_marker,
            snapshots::{
                reset_named_snapshot, restore_node_state_from_snapshot,
                save_named_node_state_snapshot, validate_snapshot_path_component,
            },
        },
        tokio_console::profile::TokioConsoleProfileNode,
    },
    utils::{
        display_last_path_components, extract_child_dir_name, matching_child_dirs,
        node_wallet_keys_from_node_yaml, peer_id_from_node_yaml, track_progress, truncate_hash,
    },
    wallet::snapshot::{create_and_save_all_wallets_snapshot, restore_wallet_snapshot_if_present},
    world::{
        ChainInfoMap, ConfigOverride, CucumberWorld, ManualNodeConfigOverrides, NodeInfo,
        NodeWalletKey, NodeWalletKeyRole, PublicCryptarchiaEndpointPeer, WalletInfo, WalletInfoMap,
        WalletType,
    },
};

pub type NodesToStartUnordered = HashMap<String, (Vec<WalletStartInfo>, Vec<String>)>;
type NodesToStartOrdered = Vec<(String, Vec<WalletStartInfo>, Vec<String>)>;

const CHAIN_SYNC_POLL_INTERVAL: Duration = Duration::from_secs(5);
const CHAIN_SYNC_STATUS_LOG_INTERVAL: Duration = Duration::from_mins(2);

// Returns the root directory for a named snapshot.

enum AlignmentStatus {
    MissingChainInfo,
    Fork,
    Aligned,
}

#[derive(Debug, Clone)]
struct ConsensusSnapshot {
    node_name: String,
    height: u64,
    header_hash: String,
}

#[derive(Debug, Clone)]
struct MaybeSnapshot {
    height: u64,
    header_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SyncTargetStats {
    lib: String,
    tip: String,
    slot: u64,
    height: u64,
}

#[derive(Debug, Clone)]
struct PublicPeerConsensusSnapshot {
    peer_url: String,
    stats: SyncTargetStats,
}

mod blend_relay;
mod consensus;
mod lifecycle;
mod resources;
mod snapshots;
mod synchronization;

pub use blend_relay::{BlendRelayRegistry, set_blend_reachability};
pub use consensus::{
    ensure_all_nodes_agree_on_lib, nodes_converged, poll_all_nodes_and_update_consensus_cache,
};
pub use lifecycle::{
    restart_node, start_node, start_nodes_order_respecting_dependencies, stop_node,
    verify_reponsive_and_network_ready_with_timeout, wait_all_nodes_responive,
};
pub use resources::{
    ensure_fee_sponsorship_and_fork_groups_are_not_mixed, genesis_block_utxos,
    parse_genesis_wallet_tokens_row, parse_mining_wallet_resources_table_row,
    parse_wallet_resources_table_row, verify_genesis_wallet_resources_table_indexes,
    verify_mining_node_wallet_resources_table_indexes, verify_node_wallet_resources_table_indexes,
};
pub use snapshots::{
    WalletStartInfo, create_snapshot_all_nodes_with_wallet_state,
    create_snapshot_node_with_wallet_state, create_snapshots_all_nodes,
    get_cryptarchia_info_all_nodes,
};
pub use synchronization::{
    fetch_public_peer_consensus, parse_url, wait_for_all_nodes_to_be_synced_to_chain,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone)]
struct MajorityPublicSyncTarget {
    peer_urls: Vec<String>,
    stats: SyncTargetStats,
}

impl SyncTargetStats {
    fn from_cryptarchia_info(info: &CryptarchiaInfo) -> Self {
        Self {
            lib: info.lib.encode_hex::<String>(),
            tip: info.tip.encode_hex::<String>(),
            slot: info.slot.into_inner(),
            height: info.height,
        }
    }
}
