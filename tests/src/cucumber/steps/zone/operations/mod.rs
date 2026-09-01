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

use lb_common_http_client::{CommonHttpClient, Slot};
use lb_core::{
    crypto::Hash,
    mantle::{
        Note, Op, OpProof, RawMantleTx, Utxo, Value,
        gas::GasCost,
        ledger::{Inputs, NoteId, Outputs, OutputsError},
        ops::{
            OpId as _,
            channel::{
                ChannelId, MsgId,
                channel_transfer::ChannelTransferOp,
                deposit::{DepositOp, Metadata},
                inscribe::{Inscription, InscriptionOp},
                withdraw::ChannelWithdrawOp,
            },
            transfer::TransferOp,
        },
        traits::Hashable as _,
        transactions::{OpsProofs, builder::MantleTxBuilder, states::Unverified},
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_http_api_common::bodies::{
    channel::{ChannelDepositRequestBody, ChannelDepositResponseBody},
    wallet::{
        fund::WalletFundRequestBody,
        sign::{WalletSignTxZkRequestBody, WalletSignTxZkResponseBody},
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey, ZkPublicKeys, ZkSignature};
use lb_node::SignedMantleTx;
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
    InscriptionId, InscriptionInfo, PendingTx, PublishResult, SequencerChannelView,
    SequencerCheckpoint, SequencerClient, SequencerConfig, TurnNotification, TxStatus,
    TxStatusUpdate, WithdrawArg, WithdrawInputs,
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
            FinalizedOp::Deposit(_)
            | FinalizedOp::Withdraw(_)
            | FinalizedOp::Config(_)
            | FinalizedOp::ChannelTransfer(_) => None,
        })
}
use crate::{
    common::{
        chain::wait_for_transactions_inclusion, mantle_inscription::make_inscription,
        wallet::build_wallet_funded_transfer,
    },
    cucumber::world::ZoneReaderConfig,
};

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
    #[error("channel wallet request failed: {message}")]
    ChannelWallet { message: String },
    #[error("timed out waiting for a channel wallet note")]
    ChannelWalletTimeout,
    #[error("timed out waiting for zone LIB to advance")]
    LibAdvanceTimeout,
    #[error("timed out waiting for zone sequencer channel view condition: {message}")]
    ChannelViewTimeout { message: String },
    #[error("failed to find a funding note with exact value {value}")]
    MissingExactFundingNote { value: Value },
    #[error("failed to submit zone deposit: {message}")]
    SubmitDeposit { message: String },
    #[error("failed to submit zone channel split transfer: {message}")]
    SplitTransfer { message: String },
    #[error("failed to sign zone transaction: {message}")]
    SignTransaction { message: String },
    #[error("failed to build atomic zone deposit transaction: {message}")]
    BuildAtomicDeposit { message: String },
    #[error("failed to submit atomic zone deposit transaction: {message}")]
    SubmitAtomicDeposit { message: String },
    #[error("failed to submit zone withdraw transaction: {message}")]
    SubmitWithdraw { message: String },
    #[error("timed out waiting for zone withdraw to appear in the indexer")]
    WithdrawTimeout,
    #[error("failed to build custom zone transaction: {message}")]
    BuildCustomTx { message: String },
    #[error("failed to submit custom zone transaction: {message}")]
    SubmitCustomTx { message: String },
    #[error("zone sequencer event stream stopped before observing the expected event")]
    SequencerStopped,
    #[error(transparent)]
    BoundedError(#[from] lb_utils::bounded::BoundedError),
    #[error(transparent)]
    OutputsError(#[from] OutputsError),
}

/// Result of an atomic deposit scenario where a deposit and zone inscription
/// are submitted as one Mantle transaction.
pub struct AtomicZoneDepositSubmission {
    pub deposit: DepositOp,
    pub publish: PublishResult,
    pub reserved_inputs: Vec<Utxo>,
}

pub struct AtomicZoneDepositRequest {
    pub channel_id: ChannelId,
    pub funding_public_key: ZkPublicKey,
    pub available_utxos: Vec<Utxo>,
    pub amount: Value,
    pub inscription_data: Inscription,
    pub metadata: Metadata,
}

/// Result of a withdraw scenario where the zone sequencer signs the channel
/// withdraw and publishes the accompanying inscription.
pub struct ZoneWithdrawSubmission {
    pub withdraw: ChannelWithdrawOp,
    pub publish: PublishResult,
}

pub struct ZoneDeposit {
    pub deposit: DepositOp,
    pub reserved_inputs: Vec<Utxo>,
    /// The channel notes the deposit re-creates (1:1 with its inputs). Computed
    /// deterministically from the deposit's `op_id` and its input notes so a
    /// later channel transfer can spend them without waiting on the indexer.
    pub channel_notes: Vec<Utxo>,
}

/// The channel notes a deposit re-creates: one per input, same note (value +
/// pk), re-homed under the deposit's `op_id`. Matches the ledger's deposit
/// execution (`DepositOp::outputs` re-creates inputs 1:1) and the zone-sdk's
/// channel-note derivation.
fn recreated_channel_notes(deposit: &DepositOp, reserved_inputs: &[Utxo]) -> Vec<Utxo> {
    let op_id = deposit.op_id();
    reserved_inputs
        .iter()
        .enumerate()
        .map(|(index, utxo)| Utxo::new(op_id, index, utxo.note))
        .collect()
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
    pub client: SequencerClient,
    pub events: tokio::sync::broadcast::Receiver<Event>,
    pub checkpoint_rx: tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
    pub ready_rx: tokio::sync::watch::Receiver<bool>,
    pub channel_view_rx: tokio::sync::watch::Receiver<SequencerChannelView>,
    pub turn_to_write_rx: tokio::sync::watch::Receiver<TurnNotification>,
    pub tx_status_rx: tokio::sync::broadcast::Receiver<TxStatusUpdate>,
}

fn to_policy_runtime(rt: runner::Runtime) -> PolicyRuntime {
    PolicyRuntime {
        task: rt.task,
        client: rt.client,
        events: rt.event_rx,
        checkpoint_rx: rt.checkpoint_rx,
        ready_rx: rt.ready_rx,
        channel_view_rx: rt.channel_view_rx,
        turn_to_write_rx: rt.turn_to_write_rx,
        tx_status_rx: rt.tx_status_rx,
    }
}

mod atomic;
mod custom_policy;
mod deposit_policy;
mod observation;
mod policies;
mod transactions;

use atomic::{build_atomic_deposit_op, build_atomic_deposit_transfer, sign_tx_zk};
pub(super) use atomic::{publish_atomic_zone_withdraw, submit_zone_withdraw};
pub(super) use custom_policy::{CustomRepublishDeps, start_custom_republish_policy};
pub(super) use deposit_policy::{start_deposit_lifecycle_policy, start_deposit_withdraw_policy};
pub(super) use observation::{
    balance_update_payload, collect_indexed_messages, collect_indexed_messages_exactly_once,
    ensure_zone_transactions_included, keygen, parse_balance_payload, publish_message_with_retry,
    replay_finalized_history, replayed_inscription_payloads, sequencer_config,
    sequencer_config_with_pending_submit_depth, wait_for_channel_transfer_input_count,
    wait_for_channel_view, wait_for_channel_wallet_counts, wait_for_channel_wallet_note,
    wait_for_deposit, wait_for_exact_indexed_payload_count,
    wait_for_finalized_deposit_via_sequencer_and_collect_mempool_pending,
    wait_for_finalized_withdraw_via_sequencer_and_collect_mempool_pending, wait_for_lib_advance,
    wait_for_on_chain_statuses_and_collect_mempool_pending, wait_for_transactions_finalized,
    wait_for_turn_to_write, wait_for_tx_status_lifecycle, wait_for_withdraw,
};
pub(super) use policies::{
    start_balance_aware_policy, start_republish_lineage_policy, start_sequencer_event_loop,
    start_sorted_conflict_policy,
};
use transactions::build_funded_custom_tx;
pub(super) use transactions::{
    build_zone_deposit, build_zone_deposit_from_values, submit_atomic_zone_deposit,
    submit_zone_channel_split, submit_zone_deposit,
};
