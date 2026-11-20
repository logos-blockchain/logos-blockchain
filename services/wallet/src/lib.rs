pub mod api;
mod states;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    num::NonZero,
};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt as _, TryStreamExt as _};
use lb_binary_codec::canonical::BinaryDecode;
use lb_chain_service::{
    ChainServiceInfo, LibUpdate,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    block::Block,
    era::ForkDigest,
    events::Events,
    header::HeaderId,
    mantle::{
        Note, Op, OpProof, SignedOps, TxHash, Value, VerificationError,
        gas::{GasCost, GasOverflow, GasProfile},
        ledger::verification_mode::StandardMode,
        note_set::NoteSetIndex,
        notes::{NoteCommitment, NoteNullifier},
        ops::{
            ChannelInscribeProof, NoOpProof, OpRef, ZkAndEd25519Proof, ZkTransferProof,
            channel::ChannelId,
        },
        traits::{GenesisTx as _, Hashable as _, MantleTx as _, SignedMantleTx, StorageSize},
        transactions::{
            MantleTxBuilder, OpProofs, Ops, TxBuilderError,
            builder::{PriorityFee, ZkTransferOpening},
            states::Preverified,
            tx_list::ops::OpsContext,
        },
    },
    sdp::DeclarationId,
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::Fr;
use lb_key_management_system_service::{
    api::{KmsServiceApi, KmsServiceData},
    backend::{KMSBackend, preload::PreloadKMSBackend},
    keys::{
        ED25519_PUBLIC_KEY_SIZE, Ed25519Key, Ed25519PublicKey, Ed25519Signature, Key, KeyOperators,
        PayloadEncoding, SignatureEncoding, ZkKey, ZkPublicKey, secured_key::SecuredKey,
    },
    operators::zk::{
        note_encryption::{DeriveNoteEncryptionKey, OpenDeliveryMessages},
        nullifier::DeriveNullifier,
        voucher::UnsafeVoucherOperator,
    },
};
use lb_ledger::LedgerState;
use lb_log_targets::wallet;
use lb_mmr::MerklePath;
use lb_note_encryption::DeliveryMessage;
use lb_services_utils::{
    overwatch::{RecoveryData, RecoveryOperator, StorageRecoverySettings},
    wait_until_services_are_ready,
};
use lb_storage_service::{api::StorageApi, recovery::StorageRecoveryBackend};
use lb_time::era::{EpochLength, EraSchedule};
use lb_time_service::{EraEvent, TimeService, api::TimeServiceApi};
use lb_utils::bounded::BoundedError;
pub use lb_wallet::{DEFAULT_FEE_MARGIN_PERCENT, SPEND_ROOT_DEPTH};
use lb_wallet::{
    KnownNote, KnownNotes, OwnedNote, WalletBalance, WalletBlock, WalletError, WalletState,
    address::Address,
    transaction::{OpWitness, WitnessContext, op_witness_at, op_witnesses},
};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData, state::StateUpdater},
};
use rand::{RngCore as _, rngs::OsRng};
use serde::{Serialize, de::DeserializeOwned};
use strum_macros::AsRefStr;
use tokio::{
    sync::{oneshot, oneshot::Sender},
    task::JoinError,
};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};
use tracing::{debug, error, info, trace, warn};

use crate::states::{RecoveryState, ServiceState, Wallet};

type KmsBackend = PreloadKMSBackend;
type KeyId = <KmsBackend as KMSBackend>::KeyId;

const LOG_TARGET: &str = wallet::SERVICE;

/// The storage key of the leader reward notes the LIB does not hold yet.
const REWARD_NOTES_KEY: &[u8] = b"wallet/reward_notes";
const PENDING_DELIVERIES_KEY: &[u8] = b"wallet/pending_deliveries";
const NAMED_STAKING_KEYS_KEY: &[u8] = b"wallet/named_staking_keys";
// followed by the commitment of the note
const SENT_NOTE_KEY_PREFIX: &[u8] = b"wallet/sent_notes/";

#[derive(Debug, thiserror::Error)]
pub enum WalletServiceError {
    #[error("Ledger state corresponding to block {0} not found")]
    LedgerStateNotFound(HeaderId),

    #[error("Wallet state corresponding to block {0} not found")]
    FailedToFetchWalletStateForBlock(HeaderId),

    #[error("Failed to apply historical block {0} to wallet")]
    FailedToApplyBlock(HeaderId),

    #[error("Block {0} not found in storage")]
    BlockNotFoundInStorage(HeaderId),

    #[error("Events for block {0} are unavailable in storage")]
    BlockEventsUnavailable(HeaderId),

    #[error(transparent)]
    WalletError(#[from] WalletError),

    #[error("KMS API error: {0}")]
    KmsApi(DynError),

    #[error("Cryptarchia API error: {0}")]
    CryptarchiaApi(#[from] lb_chain_service::api::ApiError),

    #[error("Channel {0:?} is missing state in ledger")]
    MissingChannelState(ChannelId),

    #[error("Declaration {0:?} is missing in ledger")]
    MissingDeclaration(DeclarationId),

    #[error(transparent)]
    TxBuilder(#[from] TxBuilderError),

    #[error(transparent)]
    GasOverflow(#[from] GasOverflow),

    #[error("Transaction fee exceeded the configured max fee. tx_fee={tx_fee} > max_fee={max_fee}")]
    TxFeeExceedsMaxFee { max_fee: GasCost, tx_fee: GasCost },

    #[error("The wallet holds no key for {0:?}")]
    UnknownKey(ZkPublicKey),

    #[error("Failed to read or write the persisted state of the wallet: {0}")]
    PersistedState(DynError),

    #[error("blocking task failed: {0}")]
    TaskJoin(#[from] JoinError),

    #[error("Failed to fetch Channel Withdraw proof for op index {0} from the TxBuilder")]
    ChannelMultiSigProofNotFound(usize),

    #[error("Channel steps are proven by their holders")]
    UnsupportedSteps,

    #[error("The proofs given do not make the proof of the Operation")]
    OpProofParts,

    #[error(transparent)]
    BoundedError(#[from] BoundedError),

    #[error(transparent)]
    VerificationError(#[from] VerificationError),
    #[error("Failed to generated signature for weak public key {}", hex::encode(.0))]
    InvalidSigner([u8; ED25519_PUBLIC_KEY_SIZE]),
}

#[derive(Debug, AsRefStr)]
pub enum WalletMsg {
    GetBalance {
        tip: Option<HeaderId>,
        pk: ZkPublicKey,
        resp_tx: Sender<Result<TipResponse<Option<WalletBalance>>, WalletServiceError>>,
    },
    FundTx {
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        /// Percentage of the final mandatory fee reserved as a priority-fee
        /// reserve; only the unused reserve becomes the effective tip. `None`
        /// takes the fee margin of the wallet configuration.
        priority_fee_percent: Option<u64>,
        /// The fee the reserve is cut down to stay within, if any.
        max_tx_fee: Option<GasCost>,
        resp_tx: Sender<Result<TipResponse<MantleTxBuilder>, WalletServiceError>>,
    },
    SignTx {
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        resp_tx:
            Sender<Result<TipResponse<SignedOps<Preverified, StandardMode>>, WalletServiceError>>,
    },
    SignTxWithEd25519 {
        tx_hash: TxHash,
        pk: <Ed25519Key as SecuredKey>::PublicKey,
        resp_tx: Sender<Result<<Ed25519Key as SecuredKey>::Signature, WalletServiceError>>,
    },
    // The funding Transfer of the built Operations and its proof, None
    // without funding.
    ProveFunding {
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        resp_tx: Sender<Result<TipResponse<ProvenFunding>, WalletServiceError>>,
    },
    // Pushes a Transfer giving `owner` a note of exactly `value`.
    PrepareExactNote {
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        value: Value,
        owner: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        resp_tx: Sender<Result<TipResponse<(MantleTxBuilder, KnownNote)>, WalletServiceError>>,
    },
    OwnedNote {
        tip: Option<HeaderId>,
        commitment: NoteCommitment,
        resp_tx: Sender<Result<TipResponse<Option<OwnedNote>>, WalletServiceError>>,
    },
    GetLeaderAgedNotes {
        tip: Option<HeaderId>,
        epoch: Epoch,
        resp_tx: Sender<Result<TipResponse<Vec<AgedNote>>, WalletServiceError>>,
    },
    GetLeaderAgedNotesInfo {
        tip: Option<HeaderId>,
        resp_tx: Sender<Result<TipResponse<LeaderAgedNotesInfo>, WalletServiceError>>,
    },
    GenerateRewardKey {
        resp_tx: Sender<Result<ZkPublicKey, WalletServiceError>>,
    },
    // A leader reward note under a private random nonce, returned once persisted.
    NewLeaderReward {
        value: Value,
        public_key: ZkPublicKey,
        resp_tx: Sender<Result<Note, WalletServiceError>>,
    },
    ImportNote {
        note: Note,
        note_set: NoteSetIndex,
        resp_tx: Sender<Result<KnownNote, WalletServiceError>>,
    },
    GetKnownAddresses {
        resp_tx: Sender<Result<Vec<ZkPublicKey>, WalletServiceError>>,
    },
    // The address a sender pays `public_key` at: the key and the key its
    // note ciphertexts are encrypted to.
    GetAddress {
        public_key: ZkPublicKey,
        resp_tx: Sender<Result<Address, WalletServiceError>>,
    },
    // Keeps the delivery messages of notes paid to the wallet until a block
    // appends their commitment, and answers with how many are kept. A message
    // that arrives after its block is opened at once, as long as the wallet
    // still holds the state its block was applied to.
    ImportDeliveryMessages {
        messages: Vec<DeliveryMessage>,
        resp_tx: Sender<usize>,
    },
    // Names a staking key the wallet asked a sender to use in place of its
    // public key: a note delivered under it is opened as the wallet's own.
    NameStakingKey {
        staking_key: ZkPublicKey,
        resp_tx: Sender<Result<(), WalletServiceError>>,
    },
    // Keeps a note the wallet created for someone else, so that it can
    // deliver it again.
    RecordSentNote {
        note: Note,
        resp_tx: Sender<Result<(), WalletServiceError>>,
    },
    // A note the wallet created for someone else, by its commitment.
    SentNote {
        commitment: NoteCommitment,
        resp_tx: Sender<Result<Option<Note>, WalletServiceError>>,
    },
    // Rebuilds the wallet over the whole chain, from the genesis block, with
    // the notes it knows and the delivery messages it keeps: it finds the
    // notes whose commitment was appended before the oldest state it holds.
    Rescan {
        resp_tx: Sender<Result<(), WalletServiceError>>,
    },
    GetTxContext {
        block_id: Option<HeaderId>,
        resp_tx: Sender<Result<OpsContext, WalletServiceError>>,
    },
    SpendRoot {
        tip: Option<HeaderId>,
        resp_tx: Sender<Result<TipResponse<Fr>, WalletServiceError>>,
    },
    // Releases the notes reserved for a funded or split transaction that will
    // not be submitted, so that they fund the next request.
    ReleasePendingNotes {
        nullifiers: Vec<NoteNullifier>,
    },
}

// The built Operations and the proof of their funding Transfer.
pub type ProvenFunding = (Ops, Option<OpProof>);

#[derive(Debug)]
pub struct TipResponse<R> {
    pub tip: HeaderId,
    pub response: R,
}

// An owned note of the aged snapshot, with the key that spends it.
#[derive(Debug)]
pub struct AgedNote {
    pub note: OwnedNote,
    pub aged_path: MerklePath,
    pub key_id: KeyId,
}

/// One wallet-owned UTXO old enough to take part in the leadership lottery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaderAgedNoteInfo {
    pub commitment: NoteCommitment,
    pub note_set: NoteSetIndex,
    pub value: Value,
    /// The wallet address holding the note.
    pub public_key: ZkPublicKey,
}

/// The wallet's UTXOs that are eligible to lead at a given tip.
///
/// A note is eligible when it is present in the epoch's aged UTXO snapshot —
/// the same stake distribution the leadership proof is built against — and its
/// public key is one the wallet holds a key for. An empty `notes` means this
/// node cannot win a slot at that tip: either it owns no notes, or none of
/// them have aged into the current epoch's snapshot yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderAgedNotesInfo {
    pub notes: Vec<LeaderAgedNoteInfo>,
    /// Total value staked across `notes`, saturating.
    pub total_value: Value,
}

impl LeaderAgedNotesInfo {
    /// Number of eligible notes.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.notes.len()
    }
}

impl From<Vec<AgedNote>> for LeaderAgedNotesInfo {
    fn from(notes: Vec<AgedNote>) -> Self {
        Self::from_iter(notes.iter().map(|AgedNote { note, .. }| note))
    }
}

impl<'a> FromIterator<&'a OwnedNote> for LeaderAgedNotesInfo {
    fn from_iter<I: IntoIterator<Item = &'a OwnedNote>>(owned_notes: I) -> Self {
        let mut total_value: Value = 0;
        let notes = owned_notes
            .into_iter()
            .map(|owned| {
                total_value = total_value.saturating_add(owned.note.value);
                LeaderAgedNoteInfo {
                    commitment: owned.note.commitment(),
                    note_set: owned.note_set,
                    value: owned.note.value,
                    public_key: owned.note.public_key,
                }
            })
            .collect();
        Self { notes, total_value }
    }
}

impl WalletMsg {
    /// Returns [`HeaderId`] of the tip if the message is associated
    /// with a specific tip.
    #[must_use]
    pub const fn tip(&self) -> Option<HeaderId> {
        match self {
            Self::GetBalance { tip, .. }
            | Self::FundTx { tip, .. }
            | Self::SignTx { tip, .. }
            | Self::ProveFunding { tip, .. }
            | Self::PrepareExactNote { tip, .. }
            | Self::OwnedNote { tip, .. }
            | Self::GetLeaderAgedNotes { tip, .. }
            | Self::GetLeaderAgedNotesInfo { tip, .. }
            | Self::SpendRoot { tip, .. }
            | Self::GetTxContext { block_id: tip, .. } => *tip,
            Self::SignTxWithEd25519 { .. }
            | Self::GenerateRewardKey { .. }
            | Self::NewLeaderReward { .. }
            | Self::ImportNote { .. }
            | Self::GetKnownAddresses { .. }
            | Self::GetAddress { .. }
            | Self::ImportDeliveryMessages { .. }
            | Self::NameStakingKey { .. }
            | Self::RecordSentNote { .. }
            | Self::SentNote { .. }
            | Self::Rescan { .. }
            | Self::ReleasePendingNotes { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WalletServiceSettings {
    pub known_keys: HashMap<KeyId, ZkPublicKey>,
    pub voucher_master_key_id: KeyId,
    pub recovery_data: RecoveryData,
    /// How much LIB progress a pending note reservation survives before being
    /// evicted. Notes funded into in-flight transactions are excluded from
    /// funding until they are observed spent in a block or this many immutable
    /// blocks have passed since the reservation.
    pub pending_note_expiry_blocks: u64,
    /// The priority fee percentage the node funds its own transactions with.
    pub fee_margin_percent: u64,
    pub era_schedule: ServiceEraSchedule,
}

pub type ServiceEraSchedule = EraSchedule<WalletEraSettings>;

#[derive(Clone, Debug)]
pub struct WalletEraSettings {
    pub epoch_length: NonZero<u64>,
    pub security_param: NonZero<u32>,
    /// The fork digest of the era, which the transactions the wallet signs in
    /// it carry.
    pub fork_digest: ForkDigest,
    /// What the era's Operations cost, which the wallet funds transactions for.
    pub gas_profile: GasProfile,
}

impl EpochLength for WalletEraSettings {
    fn epoch_length(&self) -> NonZero<u64> {
        self.epoch_length
    }
}

#[must_use]
pub const fn default_pending_note_expiry_blocks() -> u64 {
    10
}

#[must_use]
pub const fn default_fee_margin_percent() -> u64 {
    DEFAULT_FEE_MARGIN_PERCENT
}

impl StorageRecoverySettings for WalletServiceSettings {
    const RECOVERY_KEY_SUFFIX: &'static [u8] = b"wallet";

    fn recovery_data(&self) -> &RecoveryData {
        &self.recovery_data
    }
}

pub struct WalletService<Kms, Cryptarchia, Tx, TimeBackend, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    initial_state: RecoveryState,
    _marker: std::marker::PhantomData<(Kms, Cryptarchia, Tx, TimeBackend)>,
}

impl<Kms, Cryptarchia, Tx, TimeBackend, RuntimeServiceId> ServiceData
    for WalletService<Kms, Cryptarchia, Tx, TimeBackend, RuntimeServiceId>
{
    type Settings = WalletServiceSettings;
    type State = RecoveryState;
    type StateOperator =
        RecoveryOperator<StorageRecoveryBackend<Self::State, Self::Settings, RuntimeServiceId>>;
    type Message = WalletMsg;
}

#[async_trait]
impl<Kms, Cryptarchia, Tx, TimeBackend, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for WalletService<Kms, Cryptarchia, Tx, TimeBackend, RuntimeServiceId>
where
    Kms: KmsServiceData<Backend = KmsBackend> + Send + Sync,
    Tx: SignedMantleTx<Preverified, StandardMode>
        + Send
        + Sync
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryDecode<Context = u32>
        + StorageSize
        + 'static,
    Cryptarchia: CryptarchiaServiceData<Tx = Tx>,
    TimeBackend: lb_time_service::backends::TimeBackend + Send + Sync + 'static,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<Cryptarchia>
        + AsServiceId<lb_storage_service::StorageService<RuntimeServiceId>>
        + AsServiceId<Kms>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + std::fmt::Debug
        + std::fmt::Display
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            service_resources_handle,
            initial_state,
            _marker: std::marker::PhantomData,
        })
    }

    async fn run(mut self) -> Result<(), DynError> {
        let Self {
            mut service_resources_handle,
            ..
        } = self;

        wait_until_services_are_ready!(
            &service_resources_handle.overwatch_handle,
            None,
            lb_storage_service::StorageService<_>,
            Kms,
            TimeService<_, _>,
            Cryptarchia
        )
        .await?;

        let settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let storage =
            StorageApi::<Tx>::from_overwatch_handle(&service_resources_handle.overwatch_handle)
                .await?;

        // Create the API wrapper for cleaner communication
        let cryptarchia_api = CryptarchiaServiceApi::<Cryptarchia>::from_overwatch_handle(
            &service_resources_handle.overwatch_handle,
        )
        .await;

        // Create KMS API for transaction signing
        let kms = KmsServiceApi::<Kms, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<Kms>()
                .await?,
        );

        // Query chain service for current state using the API
        let ChainServiceInfo {
            cryptarchia_info, ..
        } = cryptarchia_api.info().await?;

        info!(
            target: LOG_TARGET,
            tip = ?cryptarchia_info.tip,
            lib = ?cryptarchia_info.lib,
            slot = ?cryptarchia_info.slot,
            "Wallet connecting to chain"
        );

        // Subscribe to block updates using the API. Wrapped so that lag on the
        // broadcast channel surfaces as an item rather than being dropped by a
        // `select!` pattern.
        let mut new_blocks = BroadcastStream::new(cryptarchia_api.subscribe_new_blocks().await?);

        // Subscribe to LIB updates for wallet state pruning
        let mut lib_updates = BroadcastStream::new(cryptarchia_api.subscribe_lib_updates().await?);

        let mut state = Self::start_state(
            self.initial_state,
            &settings,
            &service_resources_handle.state_updater,
            &cryptarchia_api,
            &kms,
        )
        .await?;
        state.restore_reward_notes(Self::read_reward_notes(&storage).await?);
        Self::restore_delivery_records(&mut state, &storage).await?;
        let voucher_master_key_id = settings.voucher_master_key_id;

        Self::backfill_missing_blocks(
            cryptarchia_info.tip,
            &mut state,
            &storage,
            &cryptarchia_api,
            &kms,
            &settings.era_schedule,
        )
        .await?;

        // The transactions the wallet signs carry the fork digest of the era
        // the clock is in, and are funded for what that era charges: they can
        // only land in it or in a later one, even while the tip is older.
        let (current_era, mut era_ticks) =
            TimeServiceApi::<TimeService<TimeBackend, RuntimeServiceId>>::new(
                &service_resources_handle.overwatch_handle,
            )
            .await?
            .subscribe_to_eras()
            .await?;
        let mut era_in_force = settings.era_schedule.get(current_era).unwrap();

        service_resources_handle.status_updater.notify_ready();
        info!(target: LOG_TARGET, "Wallet service is ready and subscribed to blocks");

        loop {
            tokio::select! {
                Some(msg) = service_resources_handle.inbound_relay.recv() => {
                    Box::pin(Self::handle_wallet_message(msg, &mut state, &voucher_master_key_id, &storage, &cryptarchia_api, &kms, &settings.era_schedule, era_in_force.parameters)).await;
                }
                Some(EraEvent::NewEra { era: new_era, .. }) = era_ticks.next() => {
                    if new_era > era_in_force.era {
                        era_in_force = settings.era_schedule.get(new_era).unwrap();
                    }
                }
                // A skipped block shows up as an unknown parent on the next
                // one and is backfilled from there; the log line is what tells
                // an operator why the wallet fell behind.
                Some(event) = new_blocks.next() => match event {
                    Ok(event) => {
                        Self::handle_new_block(event.block_id, &mut state, &storage, &cryptarchia_api, &kms, &settings.era_schedule).await;
                        Self::store_pending_deliveries(&mut state, &storage).await;
                    }
                    Err(BroadcastStreamRecvError::Lagged(skipped)) => warn_lagged("new-block", skipped),
                },
                Some(lib_update) = lib_updates.next() => match lib_update {
                    Ok(lib_update) => {
                        Self::handle_lib_update(&lib_update, &storage, &mut state, &cryptarchia_api, &kms, &settings.era_schedule).await;
                        Self::store_pending_deliveries(&mut state, &storage).await;
                    }
                    Err(BroadcastStreamRecvError::Lagged(skipped)) => warn_lagged("LIB", skipped),
                },
            }
        }
    }
}

/// The wallet fell `skipped` items behind one of its chain subscriptions. Only
/// the latest LIB matters, and a skipped block is backfilled from the next one,
/// so this is a diagnostic rather than an error.
fn warn_lagged(stream: &str, skipped: u64) {
    warn!(target: LOG_TARGET, stream, skipped, "Wallet fell behind a chain subscription");
}

impl<Kms, Cryptarchia, Tx, TimeBackend, RuntimeServiceId>
    WalletService<Kms, Cryptarchia, Tx, TimeBackend, RuntimeServiceId>
where
    Kms: KmsServiceData<Backend = KmsBackend>,
    Tx: SignedMantleTx<Preverified, StandardMode>
        + Send
        + Sync
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryDecode<Context = u32>
        + StorageSize
        + 'static,
    Cryptarchia: CryptarchiaServiceData<Tx = Tx> + Send + 'static,
    RuntimeServiceId:
        AsServiceId<Cryptarchia> + AsServiceId<Kms> + std::fmt::Debug + std::fmt::Display + Sync,
{
    async fn msg_tip_or_latest(
        msg_tip: Option<HeaderId>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    ) -> Result<HeaderId, WalletServiceError> {
        if let Some(tip) = msg_tip {
            Ok(tip)
        } else {
            let ChainServiceInfo {
                cryptarchia_info, ..
            } = cryptarchia.info().await?;
            Ok(cryptarchia_info.tip)
        }
    }

    async fn ledger_state_at(
        tip: HeaderId,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    ) -> Result<LedgerState, WalletServiceError> {
        cryptarchia
            .get_ledger_state(tip)
            .await?
            .ok_or(WalletServiceError::LedgerStateNotFound(tip))
    }

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "A message reads the wallet's state, its services and the era in force"
    )]
    async fn handle_wallet_message(
        msg: WalletMsg,
        state: &mut ServiceState<'_>,
        voucher_master_key_id: &KeyId,
        storage: &StorageApi<Tx>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        era_schedule: &ServiceEraSchedule,
        era_settings: &WalletEraSettings,
    ) {
        if let Err(err) =
            Self::backfill_if_not_in_sync(msg.tip(), state, storage, cryptarchia, kms, era_schedule)
                .await
        {
            warn!(
                target: LOG_TARGET,
                message_kind = msg.as_ref(),
                requested_tip = ?msg.tip(),
                err = ?err,
                "Failed backfilling wallet to message tip; continuing to process the message"
            );
        }

        match msg {
            WalletMsg::GetBalance { tip, pk, resp_tx } => {
                Self::handle_get_balance(tip, pk, resp_tx, state.wallet(), cryptarchia).await;
            }
            WalletMsg::FundTx {
                tip,
                tx_builder,
                change_pk,
                funding_pks,
                priority_fee_percent,
                max_tx_fee,
                resp_tx,
            } => {
                // Like a funding that fails, a request that fails before
                // funding releases the notes `tx_builder`
                // spends, such as a split.
                let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
                    Ok(tip) => tip,
                    Err(err) => {
                        state.release_pending_notes(tx_builder.nullifiers_consumed());
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };

                // Fetch the tx context (gas prices) fresh at fund time. It is
                // tip-dependent, so a builder handed to us earlier must be
                // funded against the current context rather
                // than a stale one.
                let context = match Self::ledger_state_at(tip, cryptarchia).await {
                    Ok(ledger) => ledger.tx_context(era_settings.gas_profile),
                    Err(err) => {
                        state.release_pending_notes(tx_builder.nullifiers_consumed());
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };

                let change_nonce = random_nonce();
                let funded = match state.fund_tx(
                    tip,
                    &tx_builder,
                    change_pk,
                    change_nonce,
                    funding_pks,
                    &context,
                    PriorityFee {
                        percent: priority_fee_percent.unwrap_or_else(|| state.fee_margin_percent()),
                        max_fee: max_tx_fee,
                    },
                ) {
                    Ok(funded) => funded,
                    Err(err) => {
                        Self::send_err(resp_tx, WalletServiceError::from(err));
                        return;
                    }
                };

                if let Err(err) =
                    Self::add_change_note(&funded, change_pk, change_nonce, state, kms).await
                {
                    state.release_pending_notes(funded.nullifiers_consumed());
                    Self::send_err(resp_tx, err);
                    return;
                }

                let funded_notes: Vec<NoteNullifier> = funded.nullifiers_consumed().collect();
                state.reserve_pending_notes(funded_notes.iter().copied());

                if resp_tx
                    .send(Ok(TipResponse {
                        tip,
                        response: funded,
                    }))
                    .is_err()
                {
                    state.release_pending_notes(funded_notes);
                    debug!(target: LOG_TARGET, "Failed to respond to FundTx");
                }
            }
            WalletMsg::SignTx {
                tip,
                tx_builder,
                resp_tx,
            } => {
                let funded_notes: Vec<NoteNullifier> = tx_builder.nullifiers_consumed().collect();

                let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
                    Ok(tip) => tip,
                    Err(err) => {
                        state.release_pending_notes(funded_notes);
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };

                let ledger = match Self::ledger_state_at(tip, cryptarchia).await {
                    Ok(ledger) => ledger,
                    Err(err) => {
                        state.release_pending_notes(funded_notes);
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };

                let resp = Self::sign_tx(
                    tx_builder,
                    era_settings.fork_digest,
                    tip,
                    &ledger,
                    kms,
                    state.wallet(),
                )
                .await
                .map(|signed_tx| TipResponse {
                    tip,
                    response: signed_tx,
                });

                let signing_failed = resp.is_err();
                let response_delivered = resp_tx.send(resp).is_ok();

                if signing_failed || !response_delivered {
                    state.release_pending_notes(funded_notes);
                }

                if !response_delivered {
                    debug!(target: LOG_TARGET, "Failed to respond to SignTx");
                }
            }
            WalletMsg::ProveFunding {
                tip,
                tx_builder,
                resp_tx,
            } => {
                let funded_notes: Vec<NoteNullifier> = tx_builder.nullifiers_consumed().collect();

                let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
                    Ok(tip) => tip,
                    Err(err) => {
                        state.release_pending_notes(funded_notes);
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };

                let ledger = match Self::ledger_state_at(tip, cryptarchia).await {
                    Ok(ledger) => ledger,
                    Err(err) => {
                        state.release_pending_notes(funded_notes);
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };

                let resp = Self::prove_funding(
                    tx_builder,
                    era_settings.fork_digest,
                    tip,
                    &ledger,
                    kms,
                    state.wallet(),
                )
                .await
                .map(|response| TipResponse { tip, response });

                let proving_failed = resp.is_err();
                let response_delivered = resp_tx.send(resp).is_ok();

                if proving_failed || !response_delivered {
                    state.release_pending_notes(funded_notes);
                }

                if !response_delivered {
                    debug!(target: LOG_TARGET, "Failed to respond to ProveFunding");
                }
            }
            WalletMsg::PrepareExactNote {
                tip,
                tx_builder,
                value,
                owner,
                funding_pks,
                resp_tx,
            } => {
                let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
                    Ok(tip) => tip,
                    Err(err) => {
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };

                let (tx_builder, exact_note, split_notes) = match Self::prepare_exact_note(
                    tip,
                    tx_builder,
                    value,
                    owner,
                    funding_pks,
                    state,
                    kms,
                )
                .await
                {
                    Ok(prepared) => prepared,
                    Err(err) => {
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };
                state.reserve_pending_notes(split_notes.iter().copied());

                if resp_tx
                    .send(Ok(TipResponse {
                        tip,
                        response: (tx_builder, exact_note),
                    }))
                    .is_err()
                {
                    state.release_pending_notes(split_notes);
                    debug!(target: LOG_TARGET, "Failed to respond to PrepareExactNote");
                }
            }
            WalletMsg::OwnedNote {
                tip,
                commitment,
                resp_tx,
            } => {
                let response =
                    Self::owned_note_at(tip, &commitment, state.wallet(), cryptarchia).await;
                if resp_tx.send(response).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to OwnedNote");
                }
            }
            WalletMsg::SignTxWithEd25519 {
                tx_hash,
                pk,
                resp_tx,
            } => {
                let result = Self::sign_ed25519(tx_hash, pk, kms).await;
                if resp_tx.send(result).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to SignTxWithEd25519");
                }
            }
            WalletMsg::GetLeaderAgedNotes {
                tip,
                epoch,
                resp_tx,
            } => {
                let response =
                    Self::leader_aged_notes_for_epoch(tip, epoch, state.wallet(), cryptarchia)
                        .await;
                if resp_tx.send(response).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to GetLeaderAgedNotes");
                }
            }
            WalletMsg::GetLeaderAgedNotesInfo { tip, resp_tx } => {
                let response = Self::leader_aged_notes_at(tip, state.wallet(), cryptarchia)
                    .await
                    .map(|TipResponse { tip, response }| TipResponse {
                        tip,
                        response: LeaderAgedNotesInfo::from(response),
                    });
                if resp_tx.send(response).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to GetLeaderAgedNotesInfo");
                }
            }
            WalletMsg::GenerateRewardKey { resp_tx } => {
                let index = state.next_reward_key_index();
                let result =
                    Self::register_reward_key(state, voucher_master_key_id.clone(), index, kms)
                        .await;
                if resp_tx.send(result).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to GenerateRewardKey");
                }
            }
            WalletMsg::NewLeaderReward {
                value,
                public_key,
                resp_tx,
            } => {
                let result = Self::new_leader_reward(value, public_key, state, storage, kms).await;
                if resp_tx.send(result).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to NewLeaderReward");
                }
            }
            WalletMsg::ImportNote {
                note,
                note_set,
                resp_tx,
            } => {
                let result =
                    Self::import_note(note, note_set, state, storage, kms, era_schedule).await;
                Self::store_pending_deliveries(state, storage).await;
                if resp_tx.send(result).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to ImportNote");
                }
            }
            WalletMsg::GetKnownAddresses { resp_tx } => {
                Self::get_known_addresses(state.wallet(), resp_tx);
            }
            WalletMsg::GetAddress {
                public_key,
                resp_tx,
            } => {
                let result = Self::address(public_key, state, kms).await;
                if resp_tx.send(result).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to GetAddress");
                }
            }
            WalletMsg::ImportDeliveryMessages { messages, resp_tx } => {
                let kept = state.add_pending_deliveries(messages);
                let pending = state.pending_delivery_commitments();
                if kept > 0
                    && let Err(error) =
                        Self::adopt_late_notes(&pending, state, storage, kms, era_schedule).await
                {
                    // The messages stay pending: a later import tries again.
                    warn!(
                        target: LOG_TARGET,
                        err = %error,
                        "Failed to adopt the notes of late delivery messages"
                    );
                }
                Self::store_pending_deliveries(state, storage).await;
                if resp_tx.send(kept).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to ImportDeliveryMessages");
                }
            }
            WalletMsg::NameStakingKey {
                staking_key,
                resp_tx,
            } => {
                let result = if state.name_staking_key(staking_key) {
                    storage
                        .store_confirmed(
                            Bytes::from_static(NAMED_STAKING_KEYS_KEY),
                            state.named_staking_keys().to_vec(),
                        )
                        .await
                        .map_err(WalletServiceError::PersistedState)
                } else {
                    Ok(())
                };
                if resp_tx.send(result).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to NameStakingKey");
                }
            }
            WalletMsg::RecordSentNote { note, resp_tx } => {
                let result = storage
                    .store_confirmed(sent_note_key(&note.commitment()), note)
                    .await
                    .map_err(WalletServiceError::PersistedState);
                if resp_tx.send(result).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to RecordSentNote");
                }
            }
            WalletMsg::SentNote {
                commitment,
                resp_tx,
            } => {
                let result = storage
                    .load(sent_note_key(&commitment))
                    .await
                    .map_err(WalletServiceError::PersistedState);
                if resp_tx.send(result).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to SentNote");
                }
            }
            WalletMsg::Rescan { resp_tx } => {
                let result = Self::rescan(state, storage, cryptarchia, kms, era_schedule).await;
                Self::store_pending_deliveries(state, storage).await;
                if resp_tx.send(result).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to Rescan");
                }
            }
            WalletMsg::GetTxContext { block_id, resp_tx } => {
                Self::get_tx_context(block_id, resp_tx, cryptarchia, era_settings.gas_profile)
                    .await;
            }
            WalletMsg::SpendRoot { tip, resp_tx } => {
                let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
                    Ok(tip) => tip,
                    Err(err) => {
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };
                let response = state
                    .wallet()
                    .spend_root(tip)
                    .map(|response| TipResponse { tip, response })
                    .map_err(WalletServiceError::from);
                if resp_tx.send(response).is_err() {
                    debug!(target: LOG_TARGET, "Failed to respond to SpendRoot");
                }
            }
            WalletMsg::ReleasePendingNotes { nullifiers } => {
                state.release_pending_notes(nullifiers);
            }
        }
    }

    async fn handle_get_balance(
        tip: Option<HeaderId>,
        pk: ZkPublicKey,
        resp_tx: Sender<Result<TipResponse<Option<WalletBalance>>, WalletServiceError>>,
        wallet: &Wallet,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    ) {
        let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
            Ok(tip) => tip,
            Err(err) => {
                Self::send_err(resp_tx, err);
                return;
            }
        };

        let resp = wallet
            .balance(tip, pk)
            .map_err(WalletServiceError::WalletError)
            .map(|balance| TipResponse {
                tip,
                response: balance,
            });

        if resp_tx.send(resp).is_err() {
            debug!(target: LOG_TARGET, "Failed to respond to GetBalance");
        }
    }

    async fn sign_tx(
        tx_builder: MantleTxBuilder,
        fork_digest: ForkDigest,
        tip: HeaderId,
        tip_ledger: &LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        wallet: &Wallet,
    ) -> Result<SignedOps<Preverified, StandardMode>, WalletServiceError> {
        let mut channel_multi_sig_proofs = tx_builder.channel_multi_sig_proofs().clone();
        let openings = tx_builder.openings();
        let ops = tx_builder.build(fork_digest)?;
        let tx_hash = ops.hash();

        let mut zk_proofs = HashMap::new();
        for witness in Self::witnesses_at(&ops, &openings, tip, tip_ledger, wallet)? {
            zk_proofs.insert(witness.op_index, Self::prove_witness(witness, kms).await?);
        }

        let mut op_proofs = OpProofs::empty();
        for (i, op) in ops.op_refs().into_iter().enumerate() {
            let op_proof = if let OpRef::ChannelConfig(_) = op {
                let proof = channel_multi_sig_proofs
                    .remove(&i)
                    .ok_or(WalletServiceError::ChannelMultiSigProofNotFound(i))?;
                OpProof::ChannelMultiSigProof(proof)
            } else {
                let signature = Self::op_signature(tx_hash, &op, tip_ledger, kms).await?;
                op_proof(&op, zk_proofs.remove(&i), signature)?
            };
            op_proofs.try_push(op_proof)?;
        }

        let signed_mantle_tx = SignedOps::from_parts(ops, op_proofs)
            .expect("SignedOps should be valid with the provided proofs.")
            .preverify()?;

        Ok(signed_mantle_tx)
    }

    // The built Operations and the proof of their funding Transfer, the last
    // Operation, when the builder funds them.
    async fn prove_funding(
        tx_builder: MantleTxBuilder,
        fork_digest: ForkDigest,
        tip: HeaderId,
        tip_ledger: &LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        wallet: &Wallet,
    ) -> Result<(Ops, Option<OpProof>), WalletServiceError> {
        let funds = !tx_builder.funding_inputs().is_empty();
        let openings = tx_builder.openings();
        let ops = tx_builder.build(fork_digest)?;
        let Some(funding_index) = ops.len().checked_sub(1).filter(|_| funds) else {
            return Ok((ops, None));
        };

        // The other Operations are left for the caller to prove
        let witness = op_witness_at(
            &wallet.spend_state(tip)?,
            wallet.known_notes(),
            &ops,
            &openings,
            &Self::witness_context(&ops, tip_ledger),
            ops.hash().to_fr(),
            funding_index,
        )?;
        let proof = Self::prove_witness(witness, kms).await?;
        Ok((ops, Some(OpProof::ZkTransfer(proof))))
    }

    // The ZkTransfer witnesses of `ops`, from the wallet's spend state at
    // `tip`.
    fn witnesses_at(
        ops: &Ops,
        openings: &BTreeMap<usize, ZkTransferOpening>,
        tip: HeaderId,
        tip_ledger: &LedgerState,
        wallet: &Wallet,
    ) -> Result<Vec<OpWitness>, WalletServiceError> {
        Ok(op_witnesses(
            &wallet.spend_state(tip)?,
            wallet.known_notes(),
            ops,
            openings,
            &Self::witness_context(ops, tip_ledger),
            ops.hash().to_fr(),
        )?)
    }

    fn witness_context(ops: &Ops, tip_ledger: &LedgerState) -> WitnessContext {
        let channels = tip_ledger.mantle_ledger().channels();
        let channel_note_sets = ops
            .iter()
            .filter_map(|op| match op {
                Op::ChannelDeposit(deposit) => Some(deposit.channel_id),
                Op::ChannelWithdraw(withdraw) => Some(withdraw.channel_id),
                Op::ChannelInscribe(inscription) => Some(inscription.channel_id),
                _ => None,
            })
            .filter_map(|channel_id| {
                channels
                    .note_set_index(channel_id)
                    .ok()
                    .map(|note_set| (channel_id, note_set))
            })
            .collect();
        WitnessContext {
            channel_note_sets,
            pow_epoch_reward: tip_ledger.mantle_ledger().pow.epoch_reward(),
        }
    }

    // Proves `witness` with the keys of its signers, in input order.
    async fn prove_witness(
        witness: OpWitness,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<ZkTransferProof, WalletServiceError> {
        // Use hex-encoded public key as key_id for now
        let key_ids = witness
            .signers
            .iter()
            .map(|pk| hex::encode(lb_groth16::fr_to_bytes(pk.as_fr())))
            .collect();

        // Pinned to keep the future off the stack: a KMS message can carry a
        // ZkTransfer witness.
        let signature = Box::pin(kms.sign_multiple(key_ids, PayloadEncoding::Zk(witness.witness)))
            .await
            .map_err(WalletServiceError::KmsApi)?;

        let SignatureEncoding::Zk(proof) = signature else {
            return Err(WalletServiceError::KmsApi(
                "Expected ZkTransfer proof".into(),
            ));
        };

        Ok(ZkTransferProof::new(proof))
    }

    // The Ed25519 signature of `op`: by the inscription signer, or by the
    // provider of the declaration.
    async fn op_signature(
        tx_hash: TxHash,
        op: &OpRef<'_>,
        tip_ledger: &LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<Option<Ed25519Signature>, WalletServiceError> {
        let signer = match op {
            OpRef::ChannelInscribe(inscribe_op) => {
                let Ok(signer) = Ed25519PublicKey::try_from(inscribe_op.signer) else {
                    return Err(WalletServiceError::InvalidSigner(
                        inscribe_op.signer.to_bytes(),
                    ));
                };
                signer
            }
            OpRef::SDPDeclare(declare_op) => declare_op.provider_id.0,
            OpRef::SDPWithdraw(withdraw_op) => {
                Self::declaration_provider(&withdraw_op.declaration_id, tip_ledger)?
            }
            OpRef::SDPActive(active_op) => {
                Self::declaration_provider(&active_op.declaration_id, tip_ledger)?
            }
            OpRef::ChannelConfig(_)
            | OpRef::ChannelDeposit(_)
            | OpRef::ChannelWithdraw(_)
            | OpRef::Transfer(_)
            | OpRef::ClaimPowReward(_)
            | OpRef::ProgramRegister(_)
            | OpRef::Inscribe(_) => return Ok(None),
        };
        Self::sign_ed25519(tx_hash, signer, kms).await.map(Some)
    }

    fn declaration_provider(
        declaration_id: &DeclarationId,
        tip_ledger: &LedgerState,
    ) -> Result<Ed25519PublicKey, WalletServiceError> {
        let declaration = tip_ledger
            .mantle_ledger()
            .sdp_ledger()
            .get_declaration(declaration_id)
            .ok_or(WalletServiceError::MissingDeclaration(*declaration_id))?;
        Ok(declaration.provider_id.0)
    }

    async fn sign_ed25519(
        tx_hash: TxHash,
        pk: <Ed25519Key as SecuredKey>::PublicKey,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<<Ed25519Key as SecuredKey>::Signature, WalletServiceError> {
        // Use hex-encoded public key as key_id for now
        let key_id = hex::encode(pk.as_bytes());

        let payload = PayloadEncoding::Ed25519(Bytes::copy_from_slice(tx_hash.as_signing_bytes()));
        // Pinned to keep the future off the stack: a KMS message can carry a
        // ZkTransfer witness.
        let signature = Box::pin(kms.sign(key_id, payload))
            .await
            .map_err(WalletServiceError::KmsApi)?;

        let SignatureEncoding::Ed25519(ed25519_sig) = signature else {
            return Err(WalletServiceError::KmsApi(
                "Expected Ed25519 signature".into(),
            ));
        };

        Ok(ed25519_sig)
    }

    async fn owned_note_at(
        tip: Option<HeaderId>,
        commitment: &NoteCommitment,
        wallet: &Wallet,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    ) -> Result<TipResponse<Option<OwnedNote>>, WalletServiceError> {
        let tip = Self::msg_tip_or_latest(tip, cryptarchia).await?;
        let owned_note = wallet.wallet_state_at(tip)?.notes.get(commitment).cloned();
        Ok(TipResponse {
            tip,
            response: owned_note,
        })
    }

    /// Resolves the wallet-owned UTXOs that are eligible to lead at `tip`
    /// (or at the current tip when `tip` is `None`), paired with the key ids
    /// needed to build a leadership proof for them.
    async fn leader_aged_notes_at(
        tip: Option<HeaderId>,
        wallet: &Wallet,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    ) -> Result<TipResponse<Vec<AgedNote>>, WalletServiceError> {
        Self::leader_notes_with(tip, wallet, cryptarchia, WalletState::aged_notes).await
    }

    // The leader's eligible notes for a slot of `epoch`, which can be past the
    // epoch of `tip` when no block was made since.
    async fn leader_aged_notes_for_epoch(
        tip: Option<HeaderId>,
        epoch: Epoch,
        wallet: &Wallet,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    ) -> Result<TipResponse<Vec<AgedNote>>, WalletServiceError> {
        Self::leader_notes_with(tip, wallet, cryptarchia, |state| state.aged_notes_at(epoch)).await
    }

    async fn leader_notes_with(
        tip: Option<HeaderId>,
        wallet: &Wallet,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
        aged_notes: impl FnOnce(&WalletState) -> Vec<(OwnedNote, MerklePath)>,
    ) -> Result<TipResponse<Vec<AgedNote>>, WalletServiceError> {
        let tip = Self::msg_tip_or_latest(tip, cryptarchia).await?;

        let wallet_state = wallet.wallet_state_at(tip).map_err(|err| {
            error!(target: LOG_TARGET, err = ?err, "Failed to fetch wallet state");
            WalletServiceError::FailedToFetchWalletStateForBlock(tip)
        })?;

        let eligible_notes = aged_notes(&wallet_state)
            .into_iter()
            .filter_map(|(note, aged_path)| {
                wallet
                    .known_keys()
                    .get(&note.note.public_key)
                    .map(|key_id| AgedNote {
                        note,
                        aged_path,
                        key_id: key_id.clone(),
                    })
            })
            .collect();

        Ok(TipResponse {
            tip,
            response: eligible_notes,
        })
    }

    // Derives the reward key `index` from the voucher master key and makes it
    // a key of the KMS and of the wallet.
    async fn register_reward_key(
        state: &mut ServiceState<'_>,
        master_key_id: KeyId,
        index: u64,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<ZkPublicKey, WalletServiceError> {
        let key = ZkKey::new(Self::derive_voucher_from_kms(kms, master_key_id, index).await?);
        let pk = key.to_public_key();
        let key_id = hex::encode(lb_groth16::fr_to_bytes(pk.as_fr()));
        // Pinned to keep the futures off the stack: a KMS message can carry a
        // ZkTransfer witness.
        if let Err(error) = Box::pin(kms.register(key_id.clone(), Key::Zk(key))).await {
            // The KMS refuses a key id it already holds: the key of this
            // index, registered before.
            Box::pin(kms.public_key(key_id.clone()))
                .await
                .map_err(|_| WalletServiceError::KmsApi(error))?;
        }
        state.add_known_key(pk, key_id);
        Ok(pk)
    }

    // The opening of `note` with the nullifier its key derives.
    async fn known_note(
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        key_id: KeyId,
        note: Note,
        note_set: NoteSetIndex,
    ) -> Result<KnownNote, WalletServiceError> {
        let (output_tx, output_rx) = oneshot::channel();
        // Pinned to keep the future off the stack: a KMS message can carry a
        // ZkTransfer witness.
        Box::pin(kms.execute(
            key_id,
            KeyOperators::Zk(Box::new(DeriveNullifier::new(note.commitment(), output_tx))),
        ))
        .await
        .map_err(|error| WalletServiceError::KmsApi(Box::new(error)))?;
        let nullifier = output_rx.await.map_err(|_| {
            WalletServiceError::KmsApi("KMS API did not respond with the nullifier".into())
        })?;
        Ok(KnownNote {
            note,
            note_set,
            nullifier,
        })
    }

    // The note with the nullifier the key of the wallet it is under derives.
    async fn held_note(
        note: Note,
        note_set: NoteSetIndex,
        state: &ServiceState<'_>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<KnownNote, WalletServiceError> {
        let key_id = state
            .wallet()
            .known_keys()
            .get(&note.public_key)
            .cloned()
            .ok_or(WalletServiceError::UnknownKey(note.public_key))?;
        Self::known_note(kms, key_id, note, note_set).await
    }

    // The address of `public_key`, a key of the wallet, with the note
    // encryption key the KMS derives for it.
    async fn address(
        public_key: ZkPublicKey,
        state: &ServiceState<'_>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<Address, WalletServiceError> {
        let key_id = state
            .wallet()
            .known_keys()
            .get(&public_key)
            .cloned()
            .ok_or(WalletServiceError::UnknownKey(public_key))?;
        let (output_tx, output_rx) = oneshot::channel();
        kms.execute(
            key_id,
            KeyOperators::Zk(Box::new(DeriveNoteEncryptionKey::new(output_tx))),
        )
        .await
        .map_err(|error| WalletServiceError::KmsApi(Box::new(error)))?;
        let encryption_key = output_rx.await.map_err(|_| {
            WalletServiceError::KmsApi(
                "KMS API did not respond with the note encryption key".into(),
            )
        })?;
        Ok(Address {
            public_key,
            encryption_key: Some(encryption_key),
        })
    }

    // Registers `note`, which the wallet adopts when a block appends its
    // commitment. A block the wallet applied may have appended it already:
    // that block is applied again.
    async fn import_note(
        note: Note,
        note_set: NoteSetIndex,
        state: &mut ServiceState<'_>,
        storage: &StorageApi<Tx>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        era_schedule: &ServiceEraSchedule,
    ) -> Result<KnownNote, WalletServiceError> {
        let known_note = Self::held_note(note, note_set, state, kms).await?;
        state.add_known_notes([known_note]);
        let late = HashSet::from([note.commitment()]);
        Self::adopt_late_notes(&late, state, storage, kms, era_schedule).await?;
        Ok(known_note)
    }

    // A new leader reward note under `public_key`, with a private random nonce.
    // Nothing on chain opens the note, so it is returned only once it is
    // written to storage: a crash after its block is published cannot lose
    // the reward. A note of value 0 holds nothing to lose, so the wallet
    // does not track it.
    async fn new_leader_reward(
        value: Value,
        public_key: ZkPublicKey,
        state: &mut ServiceState<'_>,
        storage: &StorageApi<Tx>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<Note, WalletServiceError> {
        let note = Note::new(value, random_nonce(), public_key);
        if value == 0 {
            return Ok(note);
        }
        let known_note = Self::held_note(note, NoteSetIndex::Ledger, state, kms).await?;
        state.add_reward_note(known_note);
        Self::write_reward_notes(state, storage).await?;
        Ok(note)
    }

    async fn write_reward_notes(
        state: &ServiceState<'_>,
        storage: &StorageApi<Tx>,
    ) -> Result<(), WalletServiceError> {
        storage
            .store_confirmed(Bytes::from_static(REWARD_NOTES_KEY), state.reward_notes())
            .await
            .map_err(WalletServiceError::PersistedState)
    }

    // Writes the pending delivery messages to storage when they changed. A
    // failed write loses nothing a sender cannot deliver again, so it is only
    // logged.
    async fn store_pending_deliveries(state: &mut ServiceState<'_>, storage: &StorageApi<Tx>) {
        let Some(messages) = state.take_changed_pending_deliveries() else {
            return;
        };
        if let Err(error) = storage
            .store_confirmed(Bytes::from_static(PENDING_DELIVERIES_KEY), messages)
            .await
        {
            warn!(
                target: LOG_TARGET,
                err = %error,
                "Failed to write the pending delivery messages to storage"
            );
        }
    }

    // The pending delivery messages and the named staking keys written before
    // a restart, known again.
    async fn restore_delivery_records(
        state: &mut ServiceState<'_>,
        storage: &StorageApi<Tx>,
    ) -> Result<(), WalletServiceError> {
        let pending: Vec<Vec<u8>> = storage
            .load(Bytes::from_static(PENDING_DELIVERIES_KEY))
            .await
            .map(Option::unwrap_or_default)
            .map_err(WalletServiceError::PersistedState)?;
        state.restore_pending_deliveries(&pending);
        let named_staking_keys: Vec<ZkPublicKey> = storage
            .load(Bytes::from_static(NAMED_STAKING_KEYS_KEY))
            .await
            .map(Option::unwrap_or_default)
            .map_err(WalletServiceError::PersistedState)?;
        for staking_key in named_staking_keys {
            state.name_staking_key(staking_key);
        }
        Ok(())
    }

    async fn read_reward_notes(storage: &StorageApi<Tx>) -> Result<KnownNotes, WalletServiceError> {
        storage
            .load(Bytes::from_static(REWARD_NOTES_KEY))
            .await
            .map(Option::unwrap_or_default)
            .map_err(WalletServiceError::PersistedState)
    }

    // Registers the change note `funded` returns to a key of the wallet.
    async fn add_change_note(
        funded: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        change_nonce: Fr,
        state: &mut ServiceState<'_>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<(), WalletServiceError> {
        let Some(key_id) = state.wallet().known_keys().get(&change_pk).cloned() else {
            return Ok(());
        };
        let change = funded
            .openings()
            .into_values()
            .flat_map(|opening| opening.outputs)
            .find(|note| note.nonce == change_nonce && note.public_key == change_pk);
        if let Some(change) = change {
            let known_note = Self::known_note(kms, key_id, change, NoteSetIndex::Ledger).await?;
            state.add_known_notes([known_note]);
        }
        Ok(())
    }

    // Pushes a Transfer giving `owner` a note of exactly `value` and the rest
    // back to `owner`, registers both notes and returns the nullifiers it
    // spends.
    async fn prepare_exact_note(
        tip: HeaderId,
        tx_builder: MantleTxBuilder,
        value: Value,
        owner: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        state: &mut ServiceState<'_>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<(MantleTxBuilder, KnownNote, Vec<NoteNullifier>), WalletServiceError> {
        let key_id = state
            .wallet()
            .known_keys()
            .get(&owner)
            .cloned()
            .ok_or(WalletServiceError::UnknownKey(owner))?;
        let nonce = random_nonce();
        let (split, opening) = state.exact_note_split(
            tip,
            &tx_builder,
            value,
            owner,
            nonce,
            random_nonce(),
            funding_pks,
        )?;
        let nullifiers = split.inputs.iter().copied().collect();

        // the exact note is output 0, the rest output 1
        let exact_note = Self::known_note(
            kms,
            key_id.clone(),
            Note::new(value, nonce, owner),
            NoteSetIndex::Ledger,
        )
        .await?;
        let mut known_notes = vec![exact_note];
        for note in opening.outputs.iter().skip(1) {
            known_notes
                .push(Self::known_note(kms, key_id.clone(), *note, NoteSetIndex::Ledger).await?);
        }

        let tx_builder = tx_builder.push_op_with_opening(Op::Transfer(split), opening)?;
        state.add_known_notes(known_notes);
        Ok((tx_builder, exact_note, nullifiers))
    }

    // Registers the notes `block` reveals to a key of the wallet, so that
    // applying the block adopts them. Returns the commitments whose delivery
    // messages were opened, to forget once the block is applied.
    async fn add_revealed_notes(
        block: &WalletBlock,
        state: &mut ServiceState<'_>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<Vec<NoteCommitment>, WalletServiceError> {
        for (note_set, note) in &block.revealed {
            let Some(key_id) = state.wallet().known_keys().get(&note.public_key).cloned() else {
                continue;
            };
            if state
                .wallet()
                .known_notes()
                .contains_key(&note.commitment())
            {
                continue;
            }
            let known_note = Self::known_note(kms, key_id, *note, *note_set).await?;
            state.add_known_notes([known_note]);
        }
        Self::add_delivered_notes(block, state, kms).await
    }

    // Opens the delivery messages kept for the commitments `block` appends,
    // with every key of the wallet, and registers the notes they deliver, so
    // that applying the block adopts them. A message is opened here and not
    // when it is received: the wallet decapsulates only the ciphertexts whose
    // commitment is in a note set. The messages stay pending: the caller
    // forgets those of the returned commitments once the block is applied, so
    // that a failure in between loses none. A message no key opens is not
    // for this wallet and is forgotten with the others.
    async fn add_delivered_notes(
        block: &WalletBlock,
        state: &mut ServiceState<'_>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<Vec<NoteCommitment>, WalletServiceError> {
        let mut opened = Vec::new();
        for update in &block.updates {
            let messages = state.pending_deliveries_of(&update.commitments);
            if messages.is_empty() {
                continue;
            }
            let key_ids = state
                .wallet()
                .known_keys()
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for key_id in key_ids {
                let (output_tx, output_rx) = oneshot::channel();
                kms.execute(
                    key_id.clone(),
                    KeyOperators::Zk(Box::new(OpenDeliveryMessages::new(
                        messages.clone(),
                        state.named_staking_keys().to_vec(),
                        output_tx,
                    ))),
                )
                .await
                .map_err(|error| WalletServiceError::KmsApi(Box::new(error)))?;
                let notes = output_rx.await.map_err(|_| {
                    WalletServiceError::KmsApi(
                        "KMS API did not respond with the delivered notes".into(),
                    )
                })?;
                for note in notes.into_iter().flatten() {
                    let known_note =
                        Self::known_note(kms, key_id.clone(), note, update.note_set).await?;
                    state.add_known_notes([known_note]);
                }
            }
            opened.extend_from_slice(&update.commitments);
        }
        Ok(opened)
    }

    // Adopts the notes of `late` commitments an applied block already
    // appended: a sender may deliver a message, or hand over a note in the
    // clear, after its transaction is in a block. Each such block is applied
    // again with its descendants, after its messages are opened, so that its
    // state tracks the note from the block that appended it. Only the blocks
    // the wallet still holds the parent state of can be applied again: a
    // message for an older commitment stays pending. The opened messages are
    // forgotten once the blocks are applied: after a failure they are still
    // pending, and a later import tries again.
    async fn adopt_late_notes(
        late: &HashSet<NoteCommitment>,
        state: &mut ServiceState<'_>,
        storage: &StorageApi<Tx>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        era_schedule: &ServiceEraSchedule,
    ) -> Result<(), WalletServiceError> {
        if late.is_empty() {
            return Ok(());
        }
        let mut applied = Vec::new();
        for header_id in state.wallet().applied_blocks() {
            let block = Self::load_block(header_id, storage).await?;
            let events = Self::load_block_events(header_id, storage).await?;
            applied.push(WalletBlock::from_block(
                &block,
                era_schedule.epoch_for_slot(block.header().slot()),
                &events,
            ));
        }
        // A commitment can be in a block of each branch of a fork.
        let blocks = blocks_delivered_late(applied, late);
        if blocks.is_empty() {
            return Ok(());
        }
        let mut opened = Vec::new();
        for block in &blocks {
            opened.extend(Self::add_revealed_notes(block, state, kms).await?);
        }
        debug!(
            target: LOG_TARGET,
            n_blocks = blocks.len(),
            "Applying blocks again to adopt late notes"
        );
        state.reapply_blocks(&blocks)?;
        state.forget_pending_deliveries(&opened);
        Ok(())
    }

    // Rebuilds the wallet over the chain of the tip, from the genesis block.
    //
    // A wallet adopts a note when its commitment is appended, and holds no
    // state older than a few blocks below the LIB, so a note it learns later
    // than that is found only by following the note sets again from the start.
    // Each block is read from storage: its pending delivery messages are
    // opened, then it is applied to the new wallet, which knows every note the
    // wallet knows. The wallet is replaced at the end, or left as it was if a
    // step fails. The cost grows with the length of the chain.
    async fn rescan(
        state: &mut ServiceState<'_>,
        storage: &StorageApi<Tx>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        era_schedule: &ServiceEraSchedule,
    ) -> Result<(), WalletServiceError> {
        // The states of the blocks further than this below the block being
        // applied are dropped as the rebuild goes.
        const PRUNE_EVERY: usize = 256;

        let genesis = cryptarchia.get_genesis_block().await?;
        let genesis_tx = genesis.genesis_tx();
        let genesis_notes = genesis_tx.initial_notes();
        let genesis_service_notes = genesis_tx
            .sdp_declarations()
            .map(|declaration| declaration.operation().service_note())
            .collect::<Vec<_>>();
        let genesis_id = genesis.header().id();

        let tip = Self::msg_tip_or_latest(None, cryptarchia).await?;
        // From the tip down to the genesis block.
        let mut headers = cryptarchia
            .get_headers(Some(tip), Some(genesis_id))
            .await
            .map_err(WalletServiceError::CryptarchiaApi)?
            .try_collect::<Vec<_>>()
            .await?;
        headers.reverse();
        let lib = state.lib();
        let Some(lib_position) = headers.iter().position(|header_id| *header_id == lib) else {
            return Err(WalletServiceError::FailedToApplyBlock(lib));
        };

        let mut rebuilt = Wallet::from_genesis(
            state.wallet().known_keys().clone(),
            state.wallet().known_notes().clone(),
            genesis_id,
            &genesis_notes,
            &genesis_service_notes,
        )?;
        let mut known_notes = state.wallet().known_notes().len();
        // The commitments whose messages were opened, forgotten once the
        // wallet is replaced.
        let mut opened = Vec::new();
        for (position, header_id) in headers.iter().enumerate() {
            if *header_id == genesis_id {
                continue;
            }
            let block = Self::load_block(*header_id, storage).await?;
            let events = Self::load_block_events(*header_id, storage).await?;
            let wallet_block = WalletBlock::from_block(
                &block,
                era_schedule.epoch_for_slot(block.header().slot()),
                &events,
            );
            // Opens the delivery messages kept for this block, and registers
            // the notes it reveals to a key of the wallet.
            opened.extend(Self::add_revealed_notes(&wallet_block, state, kms).await?);
            if state.wallet().known_notes().len() != known_notes {
                known_notes = state.wallet().known_notes().len();
                rebuilt.add_known_notes(state.wallet().known_notes().values().copied());
            }
            rebuilt
                .apply_block(&wallet_block)
                .map_err(|_| WalletServiceError::FailedToApplyBlock(*header_id))?;
            if position < lib_position && position % PRUNE_EVERY == 0 {
                rebuilt.prune_states(*header_id);
            }
        }
        rebuilt.prune_states(lib);
        info!(
            target: LOG_TARGET,
            n_blocks = headers.len(),
            "Rebuilt the wallet over the chain"
        );
        state.replace_wallet(rebuilt)?;
        state.forget_pending_deliveries(&opened);
        Ok(())
    }

    // Initializes the wallet from its persisted state at LIB, or from the
    // genesis block on a fresh start, and registers its reward keys again.
    async fn start_state<'u>(
        mut initial_state: RecoveryState,
        settings: &WalletServiceSettings,
        updater: &'u StateUpdater<Option<RecoveryState>>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<ServiceState<'u>, WalletServiceError> {
        let mut state = match initial_state.take_lib_wallet_state() {
            Some((lib, wallet_state)) => ServiceState::from_lib_wallet_state(
                initial_state,
                lib,
                wallet_state,
                settings,
                updater,
            ),
            None => Self::genesis_state(initial_state, settings, updater, cryptarchia, kms).await?,
        };

        // The reward keys derived before a restart are registered again.
        for index in 0..state.reward_key_count() {
            Self::register_reward_key(
                &mut state,
                settings.voucher_master_key_id.clone(),
                index,
                kms,
            )
            .await?;
        }
        Ok(state)
    }

    // A fresh wallet starts at the genesis block, knowing the genesis notes
    // of its keys.
    async fn genesis_state<'u>(
        state: RecoveryState,
        settings: &WalletServiceSettings,
        updater: &'u StateUpdater<Option<RecoveryState>>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<ServiceState<'u>, WalletServiceError> {
        let genesis = cryptarchia.get_genesis_block().await?;
        let genesis_tx = genesis.genesis_tx();
        let notes = genesis_tx.initial_notes();
        let service_notes = genesis_tx
            .sdp_declarations()
            .map(|declaration| declaration.operation().service_note())
            .collect::<Vec<_>>();

        let key_ids = settings
            .known_keys
            .iter()
            .map(|(key_id, pk)| (*pk, key_id))
            .collect::<HashMap<_, _>>();
        let mut known_notes = Vec::new();
        for (note_set, note) in notes
            .iter()
            .map(|note| (NoteSetIndex::Ledger, note))
            .chain(service_notes.iter().map(|note| (NoteSetIndex::Sdp, note)))
        {
            if let Some(key_id) = key_ids.get(&note.public_key) {
                known_notes.push(Self::known_note(kms, (*key_id).clone(), *note, note_set).await?);
            }
        }

        Ok(ServiceState::from_genesis(
            state,
            settings,
            genesis.header().id(),
            &notes,
            &service_notes,
            known_notes,
            updater,
        )?)
    }

    /// Derive voucher secret from KMS given master key and index.
    // TODO: Use secure KMS operator that returns `VoucherCommitment` and
    // `VoucherNullifier`
    async fn derive_voucher_from_kms(
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        key_id: KeyId,
        index: u64,
    ) -> Result<Fr, WalletServiceError> {
        let (output_tx, output_rx) = oneshot::channel();
        // Pinned to keep the future off the stack: a KMS message can carry a
        // ZkTransfer witness.
        Box::pin(kms.execute(
            key_id,
            KeyOperators::Zk(Box::new(UnsafeVoucherOperator::new(
                index.into(),
                output_tx,
            ))),
        ))
        .await
        .map_err(|error| WalletServiceError::KmsApi(Box::new(error)))?;

        output_rx.await.map_err(|_| {
            WalletServiceError::KmsApi("KMS API did not respond with voucher_commitment".into())
        })
    }

    async fn backfill_if_not_in_sync(
        tip: Option<HeaderId>,
        state: &mut ServiceState<'_>,
        storage: &StorageApi<Tx>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        era_schedule: &ServiceEraSchedule,
    ) -> Result<(), WalletServiceError> {
        let tip = Self::msg_tip_or_latest(tip, cryptarchia).await?;

        if state.wallet().has_processed_block(tip) {
            // We are already in sync with `tip`.
            return Ok(());
        }

        // The caller knows a more recent tip than the wallet.
        // To resolve this, we do a JIT backfill to try to sync the wallet with
        // cryptarchia. If we still have not caught up after the backfill, we
        // return an error to the caller
        Self::backfill_missing_blocks(tip, state, storage, cryptarchia, kms, era_schedule).await?;

        if state.wallet().has_processed_block(tip) {
            Ok(())
        } else {
            error!(target: LOG_TARGET, "Failed to backfill wallet to {tip}");
            Err(WalletServiceError::FailedToFetchWalletStateForBlock(tip))
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn handle_new_block(
        header_id: HeaderId,
        state: &mut ServiceState<'_>,
        storage: &StorageApi<Tx>,
        cryptarchia_api: &CryptarchiaServiceApi<Cryptarchia>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        era_schedule: &ServiceEraSchedule,
    ) {
        let Ok(block) = Self::load_block(header_id, storage).await.inspect_err(|e| {
            error!(
                target: LOG_TARGET,
                block_id = ?header_id,
                err = %e,
                "Failed to fetch new block and ledger for wallet"
            );
        }) else {
            return;
        };

        let Ok(events) = Self::load_block_events(header_id, storage)
            .await
            .inspect_err(|e| {
                error!(
                    target: LOG_TARGET,
                    block_id = ?header_id,
                    err = %e,
                    "Failed to load block events for wallet"
                );
            })
        else {
            return;
        };
        let wallet_block = WalletBlock::from_block(
            &block,
            era_schedule.epoch_for_slot(block.header().slot()),
            &events,
        );
        // An owned note missed here is never adopted, so the block is left
        // unapplied and is backfilled from the next one.
        let opened = match Self::add_revealed_notes(&wallet_block, state, kms).await {
            Ok(opened) => opened,
            Err(e) => {
                error!(
                    target: LOG_TARGET,
                    block_id = ?header_id,
                    err = %e,
                    "Failed to register the notes the block reveals to the wallet"
                );
                return;
            }
        };
        match state.apply_block(&wallet_block) {
            Ok(()) => {
                state.forget_pending_deliveries(&opened);
                trace!(target: LOG_TARGET, block_id = ?wallet_block.id, "Applied block to wallet");
            }
            Err(WalletError::UnknownBlock(block_id)) => {
                debug!(
                    target: LOG_TARGET,
                    block_id = ?block_id,
                    "Missing block in wallet, backfilling"
                );
                if let Err(e) = Self::backfill_missing_blocks(
                    wallet_block.id,
                    state,
                    storage,
                    cryptarchia_api,
                    kms,
                    era_schedule,
                )
                .await
                {
                    error!(
                        target: LOG_TARGET,
                        block_id = ?header_id,
                        err = %e,
                        "Failed to backfill missing block to wallet"
                    );
                }
            }
            Err(e) => {
                error!(
                    target: LOG_TARGET,
                    err = %e,
                    "unexexpected error while applying block to wallet"
                );
            }
        }
    }

    async fn load_block(
        header_id: HeaderId,
        storage: &StorageApi<Tx>,
    ) -> Result<Block<Tx>, WalletServiceError> {
        storage
            .get_block(&header_id)
            .await
            .ok_or(WalletServiceError::BlockNotFoundInStorage(header_id))
    }

    async fn load_block_events(
        header_id: HeaderId,
        storage: &StorageApi<Tx>,
    ) -> Result<Events, WalletServiceError> {
        storage
            .get_block_events(&header_id)
            .await
            .ok_or(WalletServiceError::BlockEventsUnavailable(header_id))
    }

    async fn handle_lib_update(
        lib_update: &LibUpdate,
        storage: &StorageApi<Tx>,
        state: &mut ServiceState<'_>,
        cryptarchia_api: &CryptarchiaServiceApi<Cryptarchia>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        era_schedule: &ServiceEraSchedule,
    ) {
        log_lib_update(lib_update);

        // The wallet's frontier can lag the chain's LIB: during bootstrap the
        // engine jumps the LIB (e.g. on switch-to-online) without broadcasting
        // intermediate updates, so `new_lib` may be a block the wallet never
        // applied. Backfill up to it first, mirroring `handle_new_block`, so
        // the subsequent `advance_lib` finds the `WalletState` it
        // needs.
        if !state.wallet().has_processed_block(lib_update.new_lib) {
            debug!(
                target: LOG_TARGET,
                new_lib = ?lib_update.new_lib,
                "Wallet missing new LIB block, backfilling before advancing LIB"
            );
            if let Err(e) = Self::backfill_missing_blocks(
                lib_update.new_lib,
                state,
                storage,
                cryptarchia_api,
                kms,
                era_schedule,
            )
            .await
            {
                error!(
                    target: LOG_TARGET,
                    new_lib = ?lib_update.new_lib,
                    err = %e,
                    "Failed to backfill wallet to new LIB; skipping LIB advance"
                );
                return;
            }
        }

        let new_immutable_blocks_count = lib_update.pruned_blocks.immutable_blocks.len() as u64;
        state.advance_lib(lib_update.new_lib, new_immutable_blocks_count);
        Self::forget_reward_notes_at_lib(state, storage).await;
    }

    // A reward note left in storage is only known again on a restart, so a
    // failure to forget it is not one of the LIB update.
    async fn forget_reward_notes_at_lib(state: &mut ServiceState<'_>, storage: &StorageApi<Tx>) {
        if state.forget_reward_notes_at_lib()
            && let Err(err) = Self::write_reward_notes(state, storage).await
        {
            warn!(
                target: LOG_TARGET,
                err = %err,
                "Failed to forget the reward notes the LIB holds; they stay in storage"
            );
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn backfill_missing_blocks(
        tip: HeaderId,
        state: &mut ServiceState<'_>,
        storage: &StorageApi<Tx>,
        cryptarchia_api: &CryptarchiaServiceApi<Cryptarchia>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        era_schedule: &ServiceEraSchedule,
    ) -> Result<(), WalletServiceError> {
        debug!(
            target: LOG_TARGET,
            from_tip = ?tip,
            to_state_lib = ?state.lib(),
            "backfilling missing blocks"
        );

        // Fetch block IDs in [state.lib, tip]
        let missing_headers = cryptarchia_api
            .get_headers(Some(tip), Some(state.lib()))
            .await
            .map_err(WalletServiceError::CryptarchiaApi)
            .inspect_err(|e| {
                error!(
                    target: LOG_TARGET,
                    block_id = ?tip,
                    err = %e,
                    "Failed to fetch missing headers for backfill"
                );
            })?
            .try_collect::<Vec<_>>()
            .await?;

        if !missing_headers.is_empty() {
            debug!(
                target: LOG_TARGET,
                "Backfilling wallet to tip {tip:?} with {} missing headers",
                missing_headers.len()
            );
        }

        // Load/apply blocks in order from `state.lib` to `tip`
        for header_id in missing_headers.into_iter().rev() {
            if state.wallet().has_processed_block(header_id) {
                debug!(
                    target: LOG_TARGET,
                    "Skipping already processed wallet block {header_id:?}"
                );
                continue;
            }

            let block = Self::load_block(header_id, storage).await?;
            let events = Self::load_block_events(header_id, storage).await?;
            let wallet_block = WalletBlock::from_block(
                &block,
                era_schedule.epoch_for_slot(block.header().slot()),
                &events,
            );
            let opened = Self::add_revealed_notes(&wallet_block, state, kms).await?;

            if let Err(e) = state.apply_block(&wallet_block) {
                error!(
                    target: LOG_TARGET,
                    block_id = ?header_id,
                    err = %e,
                    "Failed to apply backfill block to wallet"
                );
                return Err(WalletServiceError::FailedToApplyBlock(header_id));
            }
            state.forget_pending_deliveries(&opened);
        }

        Ok(())
    }

    fn send_err<T: std::fmt::Debug>(
        tx: Sender<Result<T, WalletServiceError>>,
        err: WalletServiceError,
    ) {
        if let Err(msg) = tx.send(Err(err)) {
            debug!(target: LOG_TARGET, msg = ?msg, "Wallet failed to send error response");
        }
    }

    fn get_known_addresses(
        wallet: &Wallet,
        tx: Sender<Result<Vec<ZkPublicKey>, WalletServiceError>>,
    ) {
        let response: Vec<_> = wallet.known_keys().keys().copied().collect();
        if let Err(e) = tx.send(Ok(response)) {
            debug!(target: LOG_TARGET, err = ?e, "Failed to send known addresses response");
        }
    }

    async fn get_tx_context(
        block_id: Option<HeaderId>,
        resp_tx: Sender<Result<OpsContext, WalletServiceError>>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
        gas_profile: GasProfile,
    ) {
        let block_id = match Self::msg_tip_or_latest(block_id, cryptarchia).await {
            Ok(block_id) => block_id,
            Err(error) => {
                Self::send_err(resp_tx, error);
                return;
            }
        };

        let ledger_state = match Self::ledger_state_at(block_id, cryptarchia).await {
            Ok(ledger_state) => ledger_state,
            Err(err) => {
                Self::send_err(resp_tx, err);
                return;
            }
        };

        let context = ledger_state.tx_context(gas_profile);
        if let Err(e) = resp_tx.send(Ok(context)) {
            debug!(target: LOG_TARGET, err = ?e, "Failed to send gas context response");
        }
    }
}

fn log_lib_update(lib_update: &LibUpdate) {
    let stale_blocks_count = lib_update.pruned_blocks.stale_blocks.len();
    let immutable_blocks_count = lib_update.pruned_blocks.immutable_blocks.len();
    if stale_blocks_count == 0 && immutable_blocks_count == 1 {
        trace!(
            target: LOG_TARGET,
            new_lib = ?lib_update.new_lib,
            stale_blocks_count,
            immutable_blocks_count,
            "Received LIB update"
        );
    } else {
        debug!(
            target: LOG_TARGET,
            new_lib = ?lib_update.new_lib,
            stale_blocks_count,
            immutable_blocks_count,
            "Received LIB update"
        );
    }
}

// A fresh nonce for a note the wallet creates.
// The storage key of the note of `commitment` the wallet created for someone
// else.
// The blocks to apply again for the delivery messages of `pending`: each block
// that appends one of these commitments, on whichever branch, and the
// descendants of such a block. `blocks` holds each block after its parent.
fn blocks_delivered_late(
    blocks: Vec<WalletBlock>,
    pending: &HashSet<NoteCommitment>,
) -> Vec<WalletBlock> {
    let mut again = HashSet::new();
    blocks
        .into_iter()
        .filter(|block| {
            let delivers = block
                .updates
                .iter()
                .flat_map(|update| &update.commitments)
                .any(|commitment| pending.contains(commitment));
            let applied_again = delivers || again.contains(&block.parent);
            if applied_again {
                again.insert(block.id);
            }
            applied_again
        })
        .collect()
}

fn sent_note_key(commitment: &NoteCommitment) -> Bytes {
    [
        SENT_NOTE_KEY_PREFIX,
        &lb_groth16::fr_to_bytes(&commitment.0),
    ]
    .concat()
    .into()
}

fn random_nonce() -> Fr {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    lb_groth16::fr_from_mod_bytes(&bytes)
}

// The proof of each Operation from its ZkTransfer and Ed25519 parts.
const fn op_proof(
    op: &OpRef<'_>,
    zk_proof: Option<ZkTransferProof>,
    signature: Option<Ed25519Signature>,
) -> Result<OpProof, WalletServiceError> {
    match (op, zk_proof, signature) {
        (OpRef::ChannelInscribe(inscription), _, _) if !inscription.mini_blocks.is_empty() => {
            Err(WalletServiceError::UnsupportedSteps)
        }
        (OpRef::ChannelInscribe(_), None, Some(signature)) => Ok(OpProof::ChannelInscribe(
            ChannelInscribeProof::without_steps(signature),
        )),
        (
            OpRef::Transfer(_) | OpRef::ChannelDeposit(_) | OpRef::ChannelWithdraw(_),
            Some(zk_proof),
            None,
        ) => Ok(OpProof::ZkTransfer(zk_proof)),
        (OpRef::SDPDeclare(_) | OpRef::SDPWithdraw(_), Some(zk_proof), Some(signature)) => Ok(
            OpProof::ZkAndEd25519Sigs(ZkAndEd25519Proof::new(zk_proof, signature)),
        ),
        (OpRef::SDPActive(_), None, Some(signature)) => Ok(OpProof::Ed25519Sig(signature)),
        (OpRef::ClaimPowReward(_) | OpRef::ProgramRegister(_) | OpRef::Inscribe(_), None, None) => {
            Ok(OpProof::None(NoOpProof))
        }
        _ => Err(WalletServiceError::OpProofParts),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fmt::{self, Display, Formatter},
        sync::Arc,
        time::Duration,
    };

    use lb_binary_codec::canonical::BinaryEncode as _;
    use lb_chain_service::{ConsensusMsg, Query};
    use lb_core::{
        block::{BlockTransactions, UncleHeaders},
        events::{BlockEvent, Event},
        mantle::{
            gas::Gas,
            note_set::{CommitmentMmr, NoteSetUpdate},
            ops::{
                sdp::{SDPActiveOp, SDPDeclareOp},
                transfer::TransferOp,
            },
        },
        proofs::leader_proof::Groth16LeaderProof,
    };
    use lb_cryptarchia_engine::Slot;
    use lb_groth16::CompressedGroth16Proof;
    use lb_key_management_system_service::KMSService;
    use lb_storage_service::api::requests::StorageMsg;
    use lb_time::era::{EraEntriesAfterGenesis, EraTiming};
    use overwatch::services::{
        relay::OutboundRelay,
        state::{NoOperator, NoState, ServiceState as _},
    };
    use time::OffsetDateTime;
    use tokio::sync::{mpsc, watch};

    use super::*;

    type TestTx = SignedOps<Preverified, StandardMode>;
    type TestKms = KMSService<KmsBackend, TestRuntimeServiceId>;
    type TestWallet = WalletService<TestKms, DummyCryptarchia, TestTx, (), TestRuntimeServiceId>;

    struct DummyCryptarchia;

    impl ServiceData for DummyCryptarchia {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ConsensusMsg<TestTx>;
    }

    #[derive(Debug)]
    struct TestRuntimeServiceId;

    impl AsServiceId<DummyCryptarchia> for TestRuntimeServiceId {
        const SERVICE_ID: Self = Self;
    }

    impl AsServiceId<TestKms> for TestRuntimeServiceId {
        const SERVICE_ID: Self = Self;
    }

    impl Display for TestRuntimeServiceId {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            write!(f, "TestRuntimeServiceId")
        }
    }

    // A transaction can be in a block of each branch of a fork when its
    // delivery message arrives. Every one of these blocks is applied again:
    // applying only the first found left the note out of the other branch.
    #[test]
    fn a_late_delivery_reapplies_the_block_of_every_branch_that_holds_its_commitment() {
        use lb_core::mantle::note_set::NoteSetUpdate;

        let id = |byte: u8| HeaderId::from([byte; 32]);
        let commitment = |value: u64| NoteCommitment(Fr::from(value));
        let block = |block: u8, parent: u8, appended: u64| WalletBlock {
            id: id(block),
            parent: id(parent),
            epoch: Epoch::from(0),
            updates: vec![NoteSetUpdate {
                note_set: NoteSetIndex::Ledger,
                nullifiers: Vec::new(),
                commitments: vec![commitment(appended)],
            }],
            revealed: Vec::new(),
        };
        // 1 -> 2 -> 4, and 1 -> 3 -> 5: blocks 2 and 3 append the delivered
        // commitment, each on its branch, and 6 is on a third branch.
        let applied = vec![
            block(2, 1, 7),
            block(3, 1, 7),
            block(4, 2, 8),
            block(5, 3, 9),
            block(6, 1, 10),
        ];

        let again = blocks_delivered_late(applied, &HashSet::from([commitment(7)]));

        assert_eq!(
            again.iter().map(|block| block.id).collect::<Vec<_>>(),
            [id(2), id(3), id(4), id(5)]
        );
    }

    /// The costs of the deployments' current gas profile.
    pub const GAS_PROFILE: GasProfile = GasProfile {
        transfer: Gas::new(10),
        program_register: Gas::new(15114),
        channel_config: Gas::new(24),
        channel_inscribe: Gas::new(26),
        channel_deposit: Gas::new(26),
        channel_withdraw: Gas::new(33),
        sdp_declare: Gas::new(1693),
        sdp_withdraw: Gas::new(0),
        sdp_active: Gas::new(15117),
        claim_pow_reward: Gas::new(1506),
        kilobyte: Gas::new(30),
        note_set: Gas::new(8961),
        proof: Gas::new(4177),
        root: Gas::new(2332),
        buffered: Gas::new(60),
        nullifier: Gas::new(1758),
        commitment: Gas::new(58),
        signature: Gas::new(557),
        user_step: Gas::new(28),
        program_deposit: Gas::new(144),
        program_step: Gas::new(13528),
        opening: Gas::new(470),
    };

    #[test]
    fn op_proof_pairs_each_operation_with_its_proof_type() {
        let zk = ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[1u8; 128]));
        let signature = Ed25519Signature::from_bytes(&[2u8; 64]);
        let transfer = TransferOp::sample();
        let declare = SDPDeclareOp::sample();
        let active = SDPActiveOp::sample();
        assert_eq!(
            op_proof(&OpRef::Transfer(&transfer), Some(zk), None).unwrap(),
            OpProof::ZkTransfer(zk)
        );
        assert_eq!(
            op_proof(&OpRef::SDPDeclare(&declare), Some(zk), Some(signature)).unwrap(),
            OpProof::ZkAndEd25519Sigs(ZkAndEd25519Proof::new(zk, signature))
        );
        assert_eq!(
            op_proof(&OpRef::SDPActive(&active), None, Some(signature)).unwrap(),
            OpProof::Ed25519Sig(signature)
        );
        assert!(op_proof(&OpRef::Transfer(&transfer), None, None).is_err());
    }

    #[tokio::test]
    async fn unavailable_events_leave_a_notified_block_for_backfill() {
        let known_note = KnownNote {
            note: Note::new(25, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64))),
            note_set: NoteSetIndex::Ledger,
            nullifier: NoteNullifier(Fr::from(3u64)),
        };
        let settings = wallet_settings(known_note.note.public_key);
        let genesis = HeaderId::from([0; 32]);
        let (sender, _receiver) = watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state = wallet_state(&settings, genesis, known_note, &updater);

        let block = create_block(genesis, 1);
        let block_id = block.header().id();
        let child = create_block(block_id, 2);
        let child_id = child.header().id();
        let commitment = known_note.note.commitment();
        let events = Events::from(Event::Block(BlockEvent::NoteSetUpdated(NoteSetUpdate {
            note_set: NoteSetIndex::Ledger,
            nullifiers: vec![],
            commitments: vec![commitment],
        })));
        let storage = storage_api([block, child], events, None);
        let cryptarchia = cryptarchia_api(genesis, block_id, child_id);
        let (kms_sender, _) = mpsc::channel(1);
        let kms = KmsServiceApi::new(OutboundRelay::new(kms_sender));

        TestWallet::handle_new_block(
            block_id,
            &mut state,
            &storage,
            &cryptarchia,
            &kms,
            &settings.era_schedule,
        )
        .await;
        assert!(!state.wallet().has_processed_block(block_id));

        // The next notification backfills the missing parent, including its events.
        TestWallet::handle_new_block(
            child_id,
            &mut state,
            &storage,
            &cryptarchia,
            &kms,
            &settings.era_schedule,
        )
        .await;

        assert!(state.wallet().has_processed_block(block_id));
        assert!(state.wallet().has_processed_block(child_id));
        let wallet_state = state.wallet().wallet_state_at(child_id).unwrap();
        assert_single_note_state(&wallet_state, known_note);
    }

    #[tokio::test]
    async fn undecodable_events_stop_backfill_until_they_can_be_loaded() {
        let known_note = KnownNote {
            note: Note::new(25, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64))),
            note_set: NoteSetIndex::Ledger,
            nullifier: NoteNullifier(Fr::from(3u64)),
        };
        let settings = wallet_settings(known_note.note.public_key);
        let genesis = HeaderId::from([0; 32]);
        let (sender, _receiver) = watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state = wallet_state(&settings, genesis, known_note, &updater);

        let block = create_block(genesis, 1);
        let block_id = block.header().id();
        let child = create_block(block_id, 2);
        let child_id = child.header().id();
        let commitment = known_note.note.commitment();
        let events = Events::from(Event::Block(BlockEvent::NoteSetUpdated(NoteSetUpdate {
            note_set: NoteSetIndex::Ledger,
            nullifiers: vec![],
            commitments: vec![commitment],
        })));
        let storage = storage_api(
            [block, child],
            events,
            Some(Bytes::from_static(b"invalid events")),
        );
        let cryptarchia = cryptarchia_api(genesis, block_id, child_id);
        let (kms_sender, _) = mpsc::channel(1);
        let kms = KmsServiceApi::new(OutboundRelay::new(kms_sender));

        let result = TestWallet::backfill_missing_blocks(
            child_id,
            &mut state,
            &storage,
            &cryptarchia,
            &kms,
            &settings.era_schedule,
        )
        .await;
        assert!(matches!(
            result,
            Err(WalletServiceError::BlockEventsUnavailable(id)) if id == block_id
        ));
        assert!(!state.wallet().has_processed_block(block_id));
        assert!(!state.wallet().has_processed_block(child_id));

        TestWallet::backfill_missing_blocks(
            child_id,
            &mut state,
            &storage,
            &cryptarchia,
            &kms,
            &settings.era_schedule,
        )
        .await
        .unwrap();

        assert!(state.wallet().has_processed_block(block_id));
        assert!(state.wallet().has_processed_block(child_id));
        let wallet_state = state.wallet().wallet_state_at(child_id).unwrap();
        assert_single_note_state(&wallet_state, known_note);
    }

    fn wallet_settings(owner: ZkPublicKey) -> WalletServiceSettings {
        WalletServiceSettings {
            known_keys: HashMap::from([("owner".into(), owner)]),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        }
    }

    fn wallet_state<'u>(
        settings: &WalletServiceSettings,
        genesis: HeaderId,
        known_note: KnownNote,
        updater: &'u StateUpdater<Option<RecoveryState>>,
    ) -> ServiceState<'u> {
        ServiceState::from_genesis(
            RecoveryState::from_settings(settings).unwrap(),
            settings,
            genesis,
            &[],
            &[],
            [known_note],
            updater,
        )
        .unwrap()
    }

    fn assert_single_note_state(state: &WalletState, known_note: KnownNote) {
        let commitment = known_note.note.commitment();
        assert_eq!(state.notes.size(), 1);
        let owned_note = state.notes.get(&commitment).unwrap();
        assert_eq!(owned_note.note, known_note.note);
        assert_eq!(owned_note.nullifier, known_note.nullifier);
        assert_eq!(
            state.balance(known_note.note.public_key).unwrap().balance,
            known_note.note.value,
        );
        let expected = CommitmentMmr::new().push(commitment).unwrap();
        assert_eq!(
            state
                .commitments
                .get(&NoteSetIndex::Ledger)
                .unwrap()
                .frontier_root(),
            expected.frontier_root(),
        );
    }

    fn era_schedule() -> ServiceEraSchedule {
        let timing = EraTiming {
            slot_duration: Duration::from_secs(1),
            epoch_length: NonZero::new(100).unwrap(),
        };
        EraSchedule::new(
            OffsetDateTime::UNIX_EPOCH,
            timing,
            EraEntriesAfterGenesis::empty(),
        )
        .unwrap()
        .map(|_| WalletEraSettings {
            epoch_length: timing.epoch_length,
            security_param: NonZero::new(5).unwrap(),
            fork_digest: ForkDigest::new([0x11; 32]),
            gas_profile: GAS_PROFILE,
        })
    }

    fn create_block(parent: HeaderId, slot: u64) -> Block<TestTx> {
        Block::create(
            parent,
            Slot::new(slot),
            UncleHeaders::empty(),
            Groth16LeaderProof::genesis(),
            BlockTransactions::empty(),
            u32::MAX,
        )
        .unwrap()
    }

    fn storage_api(
        blocks: [Block<TestTx>; 2],
        block_events: Events,
        first_event_response: Option<Bytes>,
    ) -> StorageApi<TestTx> {
        let [block, child] = blocks;
        let block_id = block.header().id();
        let child_id = child.header().id();
        let blocks = HashMap::from([
            (block_id, Bytes::from(block.encode_to_vec())),
            (child_id, Bytes::from(child.encode_to_vec())),
        ]);
        let events = HashMap::from([
            (block_id, Bytes::try_from(block_events).unwrap()),
            (child_id, Bytes::try_from(Events::new()).unwrap()),
        ]);
        let (msg_sender, mut msg_receiver) = mpsc::channel(1);
        tokio::spawn(async move {
            let mut fail_first_event_read = true;
            while let Some(msg) = msg_receiver.recv().await {
                match msg {
                    StorageMsg::GetBlock {
                        header_id,
                        response_tx,
                    } => response_tx.send(blocks.get(&header_id).cloned()).unwrap(),
                    StorageMsg::GetBlockEvents {
                        header_id,
                        response_tx,
                    } => {
                        let response = if header_id == block_id && fail_first_event_read {
                            fail_first_event_read = false;
                            first_event_response.clone()
                        } else {
                            events.get(&header_id).cloned()
                        };
                        response_tx.send(response).unwrap();
                    }
                    _ => panic!("Unexpected storage request"),
                }
            }
        });
        StorageApi::new(OutboundRelay::new(msg_sender))
    }

    fn cryptarchia_api(
        genesis: HeaderId,
        block: HeaderId,
        child: HeaderId,
    ) -> CryptarchiaServiceApi<DummyCryptarchia> {
        let (msg_sender, mut msg_receiver) = mpsc::channel(1);
        tokio::spawn(async move {
            while let Some(msg) = msg_receiver.recv().await {
                let ConsensusMsg::Query(Query::GetHeaders {
                    from_descendant,
                    to_ancestor,
                    reply_channel,
                }) = msg
                else {
                    panic!("Unexpected chain query");
                };
                assert_eq!(from_descendant, Some(child));
                assert_eq!(to_ancestor, Some(genesis));
                let headers = futures::stream::iter([Ok(child), Ok(block), Ok(genesis)]);
                assert!(reply_channel.send(Box::pin(headers)).is_ok());
            }
        });
        CryptarchiaServiceApi::new(OutboundRelay::new(msg_sender))
    }
}
