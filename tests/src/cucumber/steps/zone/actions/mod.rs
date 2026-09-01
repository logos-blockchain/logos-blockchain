use std::{collections::HashMap, num::NonZero, time::Duration};

use cucumber::gherkin::Step;
use futures::future::join_all;
use lb_common_http_client::CommonHttpClient;
use lb_core::mantle::{
    TxHash, Utxo,
    gas::GasCost,
    ops::channel::{config::Keys, deposit::Metadata, inscribe::Inscription},
};
use lb_key_management_system_service::keys::Ed25519Key;
use lb_testing_framework::NodeHttpClient;
use lb_zone_sdk::{
    adapter::NodeHttpClient as ZoneNodeHttpClient,
    sequencer::{FundingConfig, IndexedSignature, ZoneSequencer},
};
use tokio::{
    sync::broadcast,
    task::JoinHandle,
    time::{error::Elapsed, timeout},
};
use tracing::warn;

use super::{
    AtomicZoneDepositRequest, CustomRepublishDeps, DiscardedPayloads, PolicyRuntime,
    PublishDeadline, ZoneAccountBalances, ZoneDeposit, ZoneTestError, build_zone_deposit,
    build_zone_deposit_from_values, ensure_zone_transactions_included,
    errors::{log_step_error, zone_step_error},
    keygen, publish_atomic_zone_withdraw, publish_message_with_retry,
    runner::{Event, PublishResult, SequencerCheckpoint, SequencerClient},
    sequencer_config, sequencer_config_with_pending_submit_depth, start_balance_aware_policy,
    start_custom_republish_policy, start_republish_lineage_policy, start_sequencer_event_loop,
    start_sorted_conflict_policy,
    steps::DEFAULT_ZONE_SEQUENCER,
    submit_atomic_zone_deposit, submit_zone_channel_split, submit_zone_deposit,
    submit_zone_withdraw,
    tables::{ConcurrentZoneMessageRow, ZoneNodeResourcesRow, group_zone_messages_by_sequencer},
};
use crate::{
    common::{
        mantle_inscription::make_inscription, manual_cluster::wait_for_height,
        wallet::WalletReservedInputs,
    },
    cucumber::{
        error::{StepError, StepResult},
        steps::{
            TARGET,
            nodes::{
                NodesToStartUnordered, WalletStartInfo,
                config_override::set_deployment_config_override, start_node,
                start_nodes_order_respecting_dependencies,
            },
        },
        wallet::sync::current_available_utxos_for_wallet,
        world::{CucumberWorld, WalletInfo, ZoneReaderConfig},
    },
};

const ZONE_CHANNEL_WITHDRAW_THRESHOLD: u16 = 1;
const ZONE_CHANNEL_DEPOSIT_THRESHOLD: u16 = 1;
const SEQUENCER_READY_TIMEOUT: Duration = Duration::from_mins(2);
const SEQUENCER_READY_POLL_TIMEOUT: Duration = Duration::from_secs(10);
const SEQUENCER_READY_HEIGHT_ADVANCE_TIMEOUT: Duration = Duration::from_secs(30);
const ZONE_SECURITY_PARAM: u32 = 5;
// These high-volume scenarios can move storage prices before all queued
// publishes are included. Keep their test funding comfortably above the
// public 12% default so they exercise sequencing rather than fee starvation.
const ZONE_TEST_PRIORITY_FEE_PERCENT: u64 = 50;

pub(super) enum DriveMode {
    Passive {
        republish_orphans: bool,
    },
    RepublishLineage {
        planned: Vec<Inscription>,
    },
    Sorted {
        discarded: DiscardedPayloads,
    },
    BalanceAware {
        initial_balances: ZoneAccountBalances,
        planned_payloads: Vec<Inscription>,
    },
    CustomRepublish {
        deps: Box<CustomRepublishDeps>,
    },
}

impl DriveMode {
    pub(super) const fn passive() -> Self {
        Self::Passive {
            republish_orphans: false,
        }
    }

    pub(super) const fn passive_republish_orphans() -> Self {
        Self::Passive {
            republish_orphans: true,
        }
    }
}

struct PublishedZoneMessage {
    alias: String,
    payload: Inscription,
    result: PublishResult,
}

struct StartedSequencerRuntime {
    task: JoinHandle<()>,
    client: SequencerClient,
    events: broadcast::Receiver<Event>,
    checkpoint_rx: tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
    ready_rx: tokio::sync::watch::Receiver<bool>,
    channel_view_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::SequencerChannelView>,
    turn_to_write_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::TurnNotification>,
    tx_status_rx: broadcast::Receiver<lb_zone_sdk::sequencer::TxStatusUpdate>,
    discarded_payloads: Option<DiscardedPayloads>,
}

mod channel;
mod cluster;
mod publishing;
mod sequencer;

pub(super) use channel::{
    prepare_zone_channel_config, publish_atomic_zone_withdraw_transaction,
    remember_published_zone_message, save_zone_checkpoint, sign_prepared_zone_channel_config,
    stop_zone_sequencer, submit_atomic_zone_deposit_transaction,
    submit_prepared_zone_channel_config, submit_zone_channel_config,
    submit_zone_channel_split_transaction, submit_zone_deposit_transaction,
    submit_zone_multi_deposit_transaction, submit_zone_withdraw_transaction,
};
pub(super) use cluster::{
    register_zone_sequencers_with_shared_key, start_nodes_with_zone_resources,
};
pub(super) use publishing::{
    initialize_zone_indexer, publish_zone_messages, publish_zone_messages_concurrently,
};
pub(super) use sequencer::{
    start_named_sequencer, start_named_sequencer_with_pending_submit_depth,
};
