//! Zone SDK test helpers shared by Cucumber steps.
//!
//! The helpers in this module keep the feature steps focused on scenario
//! intent: start a zone-backed node, run sequencers, publish messages, observe
//! the indexer, and submit the channel operations that the zone layer relies
//! on.

use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use lb_common_http_client::{ApiBlock, CommonHttpClient, Slot};
use lb_core::mantle::{
    Op, OpProof,
    gas::GasCost,
    ops::{
        ChannelInscribeProof,
        channel::{
            ChannelId, MsgId,
            inscribe::{ChannelSteps, Inscription, InscriptionOp},
        },
    },
    traits::Hashable as _,
    transactions::{builder::MantleTxBuilder, states::Unverified},
};
use lb_http_api_common::bodies::wallet::fund::WalletFundRequestBody;
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_testing_framework::NodeHttpClient;
use lb_zone_sdk::{
    adapter::NodeHttpClient as ZoneNodeHttpClient,
    sequencer::{ZoneSequencer, channel_inscriptions},
};
use rand::{Rng as _, thread_rng};
use reqwest::Url;
use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::warn;

use super::runner::{
    self, ChannelUpdate, ChannelUpdateTx, Event, FinalizedOp, FinalizedTx, FundingConfig,
    InscriptionId, InscriptionInfo, PublishResult, SequencerChannelView, SequencerCheckpoint,
    SequencerClient, SequencerConfig, TurnNotification,
};

/// Inscriptions in the just-finalized txs — the permanent, settled part of the
/// channel. Once a payload finalizes it's on chain for good, so a policy pins
/// these and never re-homes a finalized payload when it later drops off a
/// non-canonical branch.
fn finalized_inscriptions(finalized: &[FinalizedTx]) -> impl Iterator<Item = &InscriptionInfo> {
    finalized
        .iter()
        .flat_map(|tx| tx.ops.iter())
        .filter_map(|op| match op {
            FinalizedOp::Inscription(info) => Some(info),
            FinalizedOp::Deposit(_) | FinalizedOp::Config(_) => None,
        })
}

/// The inscriptions carried by channel-update entries, in order — the
/// non-finalized counterpart of [`finalized_inscriptions`].
trait Inscriptions<'a> {
    fn inscriptions(self) -> impl Iterator<Item = &'a InscriptionInfo>;
}

impl<'a, I: IntoIterator<Item = &'a ChannelUpdateTx>> Inscriptions<'a> for I {
    fn inscriptions(self) -> impl Iterator<Item = &'a InscriptionInfo> {
        self.into_iter().filter_map(ChannelUpdateTx::inscription)
    }
}

/// The entries an update contributes to a consumer's non-finalized view: a
/// conflict's whole `canonical_chain()` (the consumer clears first), an
/// extension's `adopted` (the consumer keeps what it holds).
fn contributed(update: &ChannelUpdate) -> impl Iterator<Item = &ChannelUpdateTx> {
    let prefix = match update {
        ChannelUpdate::Conflict { common_prefix, .. } => common_prefix.as_slice(),
        ChannelUpdate::Extension { .. } => &[],
    };
    prefix.iter().chain(update.adopted())
}
use logos_blockchain_tests::common::{
    chain::{scan_chain_until, wait_for_transactions_inclusion},
    mantle_inscription::make_inscription,
};

use crate::cucumber::world::ZoneReaderConfig;

#[derive(Debug, thiserror::Error)]
pub enum ZoneTestError {
    #[error("timed out waiting for zone sequencer to accept a publish request")]
    PublishTimeout,
    #[error("zone indexer request failed: {message}")]
    Indexer { message: String },
    #[error("timed out waiting for zone indexer to return all messages")]
    IndexerTimeout,
    #[error("zone indexer returned {actual} copies of '{payload}', expected {expected}")]
    IndexedPayloadCountMismatch {
        payload: String,
        expected: usize,
        actual: usize,
    },
    #[error("timed out waiting for zone transactions to appear on the canonical chain")]
    InclusionTimeout,
    #[error("failed to fetch consensus info while checking finalized transactions: {message}")]
    Consensus { message: String },
    #[error("failed to fetch block while checking finalized transactions: {message}")]
    Block { message: String },
    #[error("timed out waiting for zone transactions to finalize")]
    FinalizationTimeout,
    #[error("timed out waiting for zone LIB to advance")]
    LibAdvanceTimeout,
    #[error("timed out waiting for zone sequencer channel view condition: {message}")]
    ChannelViewTimeout { message: String },
    #[error("failed to build custom zone transaction: {message}")]
    BuildCustomTx { message: String },
    #[error("failed to submit custom zone transaction: {message}")]
    SubmitCustomTx { message: String },
    #[error(transparent)]
    BoundedError(#[from] lb_utils::bounded::BoundedError),
}

pub type DiscardedPayloads = Arc<tokio::sync::Mutex<HashSet<Inscription>>>;
pub type ZoneAccountBalances = HashMap<String, i64>;

/// Shared deadline for a publish attempt and the matching event wait so the
/// whole operation has one timeout budget.
#[derive(Clone, Copy)]
pub struct PublishDeadline {
    started_at: Instant,
    timeout: Duration,
}

impl PublishDeadline {
    #[must_use]
    pub fn from_now(timeout: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
        }
    }

    fn is_expired(self) -> bool {
        self.started_at.elapsed() > self.timeout
    }
}

/// Bundle returned from policy starters so callers can wire the cucumber
/// world. Wraps [`runner::Runtime`] — events and checkpoints are exposed
/// uniformly across all policies because the policy runs inline on the
/// drive task; the event mpsc is purely for test observation.
pub struct PolicyRuntime {
    pub task: JoinHandle<()>,
    pub view_violation: runner::ViewViolation,
    pub client: SequencerClient,
    pub events: tokio::sync::broadcast::Receiver<Event>,
    pub checkpoint_rx: tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
    pub ready_rx: tokio::sync::watch::Receiver<bool>,
    pub channel_view_rx: tokio::sync::watch::Receiver<SequencerChannelView>,
    pub turn_to_write_rx: tokio::sync::watch::Receiver<TurnNotification>,
}

fn to_policy_runtime(rt: runner::Runtime) -> PolicyRuntime {
    PolicyRuntime {
        task: rt.task,
        view_violation: rt.view_violation,
        client: rt.client,
        events: rt.event_rx,
        checkpoint_rx: rt.checkpoint_rx,
        ready_rx: rt.ready_rx,
        channel_view_rx: rt.channel_view_rx,
        turn_to_write_rx: rt.turn_to_write_rx,
    }
}

mod custom_policy;
mod observation;
mod policies;
mod transactions;

pub(super) use custom_policy::{CustomRepublishDeps, start_custom_republish_policy};
pub(super) use observation::{
    balance_update_payload, collect_indexed_messages, collect_indexed_messages_exactly_once,
    ensure_zone_message_included, ensure_zone_transactions_included, keygen, parse_balance_payload,
    publish_message_with_retry, replay_finalized_history, replayed_inscription_payloads,
    sequencer_config, sequencer_config_with_pending_submit_depth, wait_for_channel_view,
    wait_for_exact_indexed_payload_count, wait_for_lib_advance, wait_for_transactions_finalized,
    wait_for_turn_to_write,
};
pub(super) use policies::{
    start_balance_aware_policy, start_republish_lineage_policy, start_sequencer_event_loop,
    start_sorted_conflict_policy,
};
use transactions::build_funded_custom_tx;
