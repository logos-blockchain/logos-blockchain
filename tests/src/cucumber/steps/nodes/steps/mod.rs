use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::Duration,
};

use cucumber::{gherkin::Step, given, then, when};
use lb_common_http_client::CommonHttpClient;
use lb_core::{codec::DeserializeOp as _, mantle::GenesisTime};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_libp2p::{Multiaddr, PeerId};
use lb_testing_framework::{
    USER_CONFIG_FILE,
    configs::{
        deployment::{NodeBinaryProfile, SdpFundingConfig},
        wallet::WalletAccount,
    },
    ensure_node_binary_built,
};
use time::{Duration as TimeDuration, OffsetDateTime};
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::{
    common::wallet::WalletUtxos,
    cucumber::{
        error::{StepError, StepResult},
        steps::{
            TARGET,
            cluster::{
                assert_manual_node_has_peers, connect_manual_node_to_node,
                install_local_manual_cluster, rebuild_pending_local_manual_cluster,
                stop_active_manual_cluster,
            },
            nodes::{
                NodesToStartUnordered,
                config_override::{set_deployment_config_override, set_user_config_override},
                create_snapshot_all_nodes_with_wallet_state,
                create_snapshot_node_with_wallet_state, create_snapshots_all_nodes,
                diagnostics::set_blend_diagnostic_parameter_set,
                ensure_all_nodes_agree_on_lib,
                ensure_fee_sponsorship_and_fork_groups_are_not_mixed,
                get_cryptarchia_info_all_nodes, nodes_converged, parse_genesis_wallet_tokens_row,
                parse_mining_wallet_resources_table_row, parse_url,
                parse_wallet_resources_table_row, poll_all_nodes_and_update_consensus_cache,
                restart_node,
                snapshots::validate_snapshot_path_component,
                start_node, start_nodes_order_respecting_dependencies, stop_node,
                verify_genesis_wallet_resources_table_indexes,
                verify_mining_node_wallet_resources_table_indexes,
                verify_node_wallet_resources_table_indexes,
                verify_reponsive_and_network_ready_with_timeout, wait_all_nodes_responive,
                wait_for_all_nodes_to_be_synced_to_chain,
            },
            transactions::utils::{
                create_and_submit_transaction_hashes_with_utxo_cache,
                wait_for_transactions_inclusion,
            },
        },
        utils::{
            blend_core_locator_from_node_yaml, blend_core_zk_pk_from_node_yaml,
            resolve_literal_or_env,
        },
        wallet::{
            snapshot::{
                prepare_all_wallets_snapshot, prepare_wallet_snapshot_restore_if_present,
                save_prepared_all_wallets_snapshot,
            },
            sync::{WalletSendReadiness, wait_wallet_send_ready},
        },
        world::{
            ConfigOverride, CucumberWorld, GenesisTokens, ManualClusterKind, ManualClusterSpec,
            NodeSnapshot, PublicCryptarchiaEndpointPeer,
        },
    },
    non_zero,
};

const PUBLIC_CRYPTARCHIA_ENDPOINT: &str = "public_cryptarchia_endpoint";
const PUBLIC_CRYPTARCHIA_ENDPOINT_USERNAME: &str = "username";
const PUBLIC_CRYPTARCHIA_ENDPOINT_PASSWORD: &str = "password";

mod blend;
mod configuration;
mod genesis;
mod lifecycle;
mod network;
mod snapshots;

#[cfg(test)]
mod tests;
