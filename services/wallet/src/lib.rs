pub mod api;
mod states;

use std::{collections::HashMap, path::PathBuf, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt as _, TryStreamExt as _};
use lb_chain_service::{
    Epoch, LibUpdate, Slot,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
    storage::{StorageAdapter as _, adapters::StorageAdapter},
};
use lb_core::{
    block::Block,
    header::HeaderId,
    mantle::{
        AuthenticatedMantleTx, NoteId, Op, OpProof, SignedMantleTx, Transaction as _, TxHash, Utxo,
        gas::MainnetGasConstants,
        ops::{
            channel::{ChannelId, inscribe::InscriptionOp, set_keys::SetKeysOp},
            leader_claim::{
                LeaderClaimOp, RewardsRoot, VoucherCm, VoucherNullifier, VoucherSecret,
            },
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
        },
        tx::MantleTxContext,
        tx_builder::MantleTxBuilder,
    },
    proofs::leader_claim_proof::{Groth16LeaderClaimProof, LeaderClaimPrivate, LeaderClaimPublic},
};
use lb_key_management_system_service::{
    api::{KmsServiceApi, KmsServiceData},
    backend::{KMSBackend, preload::PreloadKMSBackend},
    keys::{
        Ed25519Key, KeyOperators, PayloadEncoding, SignatureEncoding, ZkPublicKey, ZkSignature,
        secured_key::SecuredKey,
    },
    operators::zk::voucher::UnsafeVoucherOperator,
};
use lb_ledger::LedgerState;
use lb_mmr::MerklePath;
use lb_services_utils::{
    overwatch::{JsonFileBackend, RecoveryOperator, recovery::backends::FileBackendSettings},
    wait_until_services_are_ready,
};
use lb_storage_service::{api::chain::StorageChainApi, backends::StorageBackend};
use lb_wallet::{WalletBalance, WalletBlock, WalletError};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    sync::{oneshot, oneshot::Sender},
    task::JoinError,
};
use tracing::{debug, error, info, trace, warn};

use crate::states::{RecoveryState, ServiceState, Wallet};

type KmsBackend = PreloadKMSBackend;
type KeyId = <KmsBackend as KMSBackend>::KeyId;

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

    #[error(transparent)]
    WalletError(#[from] WalletError),

    #[error("KMS API error: {0}")]
    KmsApi(DynError),

    #[error("Cryptarchia API error: {0}")]
    CryptarchiaApi(#[from] lb_chain_service::api::ApiError),

    #[error("Channel {0:?} is missing state in ledger")]
    MissingChannelState(ChannelId),

    #[error("Declaration {0:?} is missing in ledger")]
    MissingDeclaration(lb_core::sdp::DeclarationId),

    #[error("Locked note {0:?} is missing in ledger")]
    MissingLockedNote(NoteId),

    #[error("Input note {0:?} is missing in ledger")]
    MissingInputNote(NoteId),

    #[error("PoC generation failed: {0:?}")]
    PoCGenerationFailed(#[from] lb_core::proofs::leader_claim_proof::Error),

    #[error("Voucher not found for the nullifier")]
    VoucherNotFound(VoucherNullifier),

    #[error("Merkle path not found for voucher_cm: {0:?}")]
    VoucherMerklePathNotFound(VoucherCm),

    #[error("blocking task failed: {0}")]
    TaskJoin(#[from] JoinError),

    #[error("Failed to fetch Channel Withdraw proof for op index {0} from the TxBuilder")]
    ChannelWithdrawProofNotFound(usize),
}

#[derive(Debug)]
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
        resp_tx: Sender<Result<TipResponse<MantleTxBuilder>, WalletServiceError>>,
    },
    SignTx {
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        resp_tx: Sender<Result<TipResponse<SignedMantleTx>, WalletServiceError>>,
    },
    SignTxWithEd25519 {
        tx_hash: TxHash,
        pk: <Ed25519Key as SecuredKey>::PublicKey,
        resp_tx: Sender<Result<<Ed25519Key as SecuredKey>::Signature, WalletServiceError>>,
    },
    SignTxWithZk {
        tx_hash: TxHash,
        pks: Vec<ZkPublicKey>,
        resp_tx: Sender<Result<ZkSignature, WalletServiceError>>,
    },
    GetLeaderAgedNotes {
        tip: Option<HeaderId>,
        resp_tx: Sender<Result<TipResponse<Vec<UtxoWithKeyId>>, WalletServiceError>>,
    },
    GenerateNewVoucherSecret {
        resp_tx: Sender<VoucherCm>,
    },
    GetClaimableVoucher {
        tip: Option<HeaderId>,
        resp_tx:
            Sender<Result<TipResponse<Option<VoucherCommitmentAndNullifier>>, WalletServiceError>>,
    },
    GetKnownAddresses {
        resp_tx: Sender<Result<Vec<ZkPublicKey>, WalletServiceError>>,
    },
    GetTxContext {
        block_id: Option<HeaderId>,
        resp_tx: Sender<Result<MantleTxContext, WalletServiceError>>,
    },
}

#[derive(Debug)]
pub struct TipResponse<R> {
    pub tip: HeaderId,
    pub response: R,
}

#[derive(Debug)]
pub struct UtxoWithKeyId {
    pub utxo: Utxo,
    pub key_id: KeyId,
}

#[derive(Debug)]
pub struct VoucherCommitmentAndNullifier {
    pub commitment: VoucherCm,
    pub nullifier: VoucherNullifier,
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
            | Self::GetLeaderAgedNotes { tip, .. }
            | Self::GetClaimableVoucher { tip, .. }
            | Self::GetTxContext { block_id: tip, .. } => *tip,
            Self::SignTxWithEd25519 { .. }
            | Self::SignTxWithZk { .. }
            | Self::GenerateNewVoucherSecret { .. }
            | Self::GetKnownAddresses { .. } => None,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WalletServiceSettings {
    pub known_keys: HashMap<KeyId, ZkPublicKey>,
    pub voucher_master_key_id: KeyId,
    pub recovery_path: PathBuf,
}

impl FileBackendSettings for WalletServiceSettings {
    fn recovery_file(&self) -> &PathBuf {
        &self.recovery_path
    }
}

pub struct WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    initial_state: RecoveryState,
    _marker: std::marker::PhantomData<(Kms, Cryptarchia, Tx, Storage)>,
}

impl<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId> ServiceData
    for WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
{
    type Settings = WalletServiceSettings;
    type State = RecoveryState;
    type StateOperator = RecoveryOperator<JsonFileBackend<Self::State, Self::Settings>>;
    type Message = WalletMsg;
}

#[async_trait]
impl<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
where
    Kms: KmsServiceData<Backend = KmsBackend> + Send + Sync,
    Tx: AuthenticatedMantleTx + Send + Sync + Clone + Eq + Serialize + DeserializeOwned + 'static,
    Cryptarchia: CryptarchiaServiceData<Tx = Tx>,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>>,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<Cryptarchia>
        + AsServiceId<lb_storage_service::StorageService<Storage, RuntimeServiceId>>
        + AsServiceId<Kms>
        + std::fmt::Debug
        + std::fmt::Display
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

        // Wait for services (except Chain) to become ready, with timeout
        wait_until_services_are_ready!(
            &service_resources_handle.overwatch_handle,
            Some(Duration::from_mins(1)),
            lb_storage_service::StorageService<_, _>,
            Kms
        )
        .await?;
        // Wait for Chain service to become ready, without timeout
        wait_until_services_are_ready!(
            &service_resources_handle.overwatch_handle,
            None,
            Cryptarchia // becomes ready after recoverying blocks
        )
        .await?;

        let settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let storage_relay = service_resources_handle
            .overwatch_handle
            .relay::<lb_storage_service::StorageService<Storage, RuntimeServiceId>>()
            .await?;

        // Create the API wrapper for cleaner communication
        let cryptarchia_api = CryptarchiaServiceApi::<Cryptarchia, _>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<Cryptarchia>()
                .await
                .expect("Failed to estabilish connection with Cryptarchia"),
        );

        // Create KMS API for transaction signing
        let kms = KmsServiceApi::<Kms, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<Kms>()
                .await?,
        );

        // Create StorageAdapter for cleaner block operations
        let storage_adapter =
            StorageAdapter::<Storage, Tx, RuntimeServiceId>::new(storage_relay).await;

        // Query chain service for current state using the API
        let chain_info = cryptarchia_api.info().await?;

        info!(
            tip = ?chain_info.tip,
            lib = ?chain_info.lib,
            slot = ?chain_info.slot,
            "Wallet connecting to chain"
        );

        // Subscribe to block updates using the API
        let mut new_block_receiver = cryptarchia_api.subscribe_new_blocks().await?;

        // Subscribe to LIB updates for wallet state pruning
        let mut lib_receiver = cryptarchia_api.subscribe_lib_updates().await?;

        let (epoch_config, consensus_config) = cryptarchia_api.get_epoch_config().await?;
        let epoch_config = EpochConfig {
            epoch_config,
            consensus_config,
        };

        // Initialize wallet from LIB and LIB LedgerState
        let lib = chain_info.lib;

        // Fetch the ledger state at LIB using the API
        let lib_ledger = cryptarchia_api
            .get_ledger_state(lib)
            .await?
            .ok_or(WalletServiceError::LedgerStateNotFound(lib))?;

        let mut state = ServiceState::new(
            self.initial_state,
            &settings,
            lib,
            &lib_ledger,
            &service_resources_handle.state_updater,
        );
        let voucher_master_key_id = settings.voucher_master_key_id;

        Self::backfill_missing_blocks(
            chain_info.tip,
            &mut state,
            &storage_adapter,
            &cryptarchia_api,
            &epoch_config,
        )
        .await?;

        service_resources_handle.status_updater.notify_ready();
        info!("Wallet service is ready and subscribed to blocks");

        loop {
            tokio::select! {
                Some(msg) = service_resources_handle.inbound_relay.recv() => {
                    Self::handle_wallet_message(msg, &mut state, &voucher_master_key_id, &storage_adapter, &cryptarchia_api, &kms, &epoch_config).await;
                }
                Ok(event) = new_block_receiver.recv() => {
                    Self::handle_new_block(event.block_id, &mut state, &storage_adapter, &cryptarchia_api, &epoch_config).await;
                }
                Ok(lib_update) = lib_receiver.recv() => {
                    Self::handle_lib_update(&lib_update, &storage_adapter, &mut state).await;
                }
            }
        }
    }
}

impl<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
    WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
where
    Kms: KmsServiceData<Backend = KmsBackend>,
    Tx: AuthenticatedMantleTx + Send + Sync + Clone + Eq + Serialize + DeserializeOwned + 'static,
    Cryptarchia: CryptarchiaServiceData<Tx = Tx> + Send + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>>,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    RuntimeServiceId:
        AsServiceId<Cryptarchia> + AsServiceId<Kms> + std::fmt::Debug + std::fmt::Display + Sync,
{
    async fn msg_tip_or_latest(
        msg_tip: Option<HeaderId>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    ) -> Result<HeaderId, WalletServiceError> {
        if let Some(tip) = msg_tip {
            Ok(tip)
        } else {
            let info = cryptarchia.info().await?;
            Ok(info.tip)
        }
    }

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn handle_wallet_message(
        msg: WalletMsg,
        state: &mut ServiceState<'_>,
        voucher_master_key_id: &KeyId,
        storage: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        epoch_config: &EpochConfig,
    ) {
        if let Err(err) =
            Self::backfill_if_not_in_sync(msg.tip(), state, storage, cryptarchia, epoch_config)
                .await
        {
            warn!(err=?err, "Failed backfilling wallet to message tip; continuing to process the message {msg:?}");
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
                resp_tx,
            } => {
                let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
                    Ok(tip) => tip,
                    Err(err) => {
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };

                let funded = match state.wallet().fund_tx::<MainnetGasConstants>(
                    tip,
                    &tx_builder,
                    change_pk,
                    funding_pks,
                ) {
                    Ok(funded) => funded,
                    Err(err) => {
                        Self::send_err(resp_tx, WalletServiceError::from(err));
                        return;
                    }
                };

                if resp_tx
                    .send(Ok(TipResponse {
                        tip,
                        response: funded,
                    }))
                    .is_err()
                {
                    error!("Failed to respond to FundTx");
                }
            }
            WalletMsg::SignTx {
                tip,
                tx_builder,
                resp_tx,
            } => {
                let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
                    Ok(tip) => tip,
                    Err(err) => {
                        Self::send_err(resp_tx, err);
                        return;
                    }
                };

                let ledger = match cryptarchia.get_ledger_state(tip).await {
                    Ok(Some(ledger)) => ledger,
                    Ok(None) => {
                        Self::send_err(resp_tx, WalletServiceError::LedgerStateNotFound(tip));
                        return;
                    }
                    Err(err) => {
                        Self::send_err(resp_tx, WalletServiceError::from(err));
                        return;
                    }
                };

                let resp = Self::sign_tx(tx_builder, tip, ledger, kms, state.wallet())
                    .await
                    .map(|signed_tx| TipResponse {
                        tip,
                        response: signed_tx,
                    });

                if resp_tx.send(resp).is_err() {
                    error!("Failed to respond to SignTx");
                }
            }
            WalletMsg::SignTxWithEd25519 {
                tx_hash,
                pk,
                resp_tx,
            } => {
                let result = Self::sign_ed25519(tx_hash, pk, kms).await;
                if resp_tx.send(result).is_err() {
                    error!("Failed to respond to SignTxWithEd25519");
                }
            }
            WalletMsg::SignTxWithZk {
                tx_hash,
                pks,
                resp_tx,
            } => {
                let result = Self::sign_zksig(tx_hash, pks, kms).await;
                if resp_tx.send(result).is_err() {
                    error!("Failed to respond to SignTxWithZk");
                }
            }
            WalletMsg::GetLeaderAgedNotes { tip, resp_tx } => {
                Self::get_leader_aged_notes(tip, resp_tx, state.wallet(), cryptarchia).await;
            }
            WalletMsg::GenerateNewVoucherSecret { resp_tx } => {
                Self::generate_new_voucher_secret(
                    state,
                    voucher_master_key_id.clone(),
                    kms,
                    resp_tx,
                )
                .await;
            }
            WalletMsg::GetClaimableVoucher { tip, resp_tx } => {
                Self::get_claimable_voucher(tip, resp_tx, state.wallet(), cryptarchia).await;
            }
            WalletMsg::GetKnownAddresses { resp_tx } => {
                Self::get_known_addresses(state.wallet(), resp_tx);
            }
            WalletMsg::GetTxContext { block_id, resp_tx } => {
                Self::get_tx_context(block_id, resp_tx, cryptarchia).await;
            }
        }
    }

    async fn handle_get_balance(
        tip: Option<HeaderId>,
        pk: ZkPublicKey,
        resp_tx: Sender<Result<TipResponse<Option<WalletBalance>>, WalletServiceError>>,
        wallet: &Wallet,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
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
            error!("Failed to respond to GetBalance");
        }
    }

    async fn sign_inscription(
        tx_hash: TxHash,
        inscribe_op: &InscriptionOp,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<OpProof, WalletServiceError> {
        let ed25519_sig = Self::sign_ed25519(tx_hash, inscribe_op.signer, kms).await?;
        Ok(OpProof::Ed25519Sig(ed25519_sig))
    }

    async fn sign_channel_deposit(
        tx_hash: TxHash,
        note_ids: Vec<NoteId>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        ledger: &LedgerState,
    ) -> Result<OpProof, WalletServiceError> {
        let input_pks = Self::resolve_note_input_pks(ledger, note_ids)?;
        let zk_sig = Self::sign_zksig(tx_hash, input_pks, kms).await?;

        Ok(OpProof::ZkSig(zk_sig))
    }

    async fn sign_channel_set_key(
        tx_hash: TxHash,
        set_keys_op: &SetKeysOp,
        ledger: &LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<OpProof, WalletServiceError> {
        let channel = ledger
            .mantle_ledger()
            .channels()
            .channel_state(&set_keys_op.channel)
            .ok_or(WalletServiceError::MissingChannelState(set_keys_op.channel))?;

        let authorized_key = channel.keys[0]; // First key is authorized key (guaranteed non-empty)
        let ed25519_sig = Self::sign_ed25519(tx_hash, authorized_key, kms).await?;

        Ok(OpProof::Ed25519Sig(ed25519_sig))
    }

    async fn sign_sdp_declare(
        tx_hash: TxHash,
        declare_op: &SDPDeclareOp,
        ledger: &LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<OpProof, WalletServiceError> {
        // For a new declaration, the note is still in the UTXOs (not yet locked).
        // We look it up from the UTXO set to get the public key for signing.
        let utxo_tree = ledger.latest_utxos();
        debug!(
            "SDPDeclare: Looking for note_id={}, utxo_tree has {} UTXOs",
            hex::encode(declare_op.locked_note_id.as_bytes()),
            utxo_tree.size()
        );
        let note = utxo_tree
            .utxos()
            .get(&declare_op.locked_note_id)
            .map(|(utxo, _)| utxo.note)
            .ok_or(WalletServiceError::MissingLockedNote(
                declare_op.locked_note_id,
            ))?;

        let zk_sig = Self::sign_zksig(tx_hash, [note.pk, declare_op.zk_id], kms).await?;
        let ed25519_sig = Self::sign_ed25519(tx_hash, declare_op.provider_id.0, kms).await?;

        Ok(OpProof::ZkAndEd25519Sigs {
            zk_sig,
            ed25519_sig,
        })
    }

    async fn sign_sdp_withdraw(
        tx_hash: TxHash,
        withdraw_op: &SDPWithdrawOp,
        ledger: &LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<OpProof, WalletServiceError> {
        let declaration = ledger
            .mantle_ledger()
            .sdp_ledger()
            .get_declaration(&withdraw_op.declaration_id)
            .ok_or(WalletServiceError::MissingDeclaration(
                withdraw_op.declaration_id,
            ))?;

        let locked_note = ledger
            .mantle_ledger()
            .locked_notes()
            .get(&declaration.locked_note_id)
            .ok_or(WalletServiceError::MissingLockedNote(
                declaration.locked_note_id,
            ))?;

        let zk_sig = Self::sign_zksig(tx_hash, [locked_note.pk, declaration.zk_id], kms).await?;

        Ok(OpProof::ZkSig(zk_sig))
    }

    async fn sign_sdp_active(
        tx_hash: TxHash,
        active_op: &SDPActiveOp,
        ledger: &LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<OpProof, WalletServiceError> {
        let declaration = ledger
            .mantle_ledger()
            .sdp_ledger()
            .get_declaration(&active_op.declaration_id)
            .ok_or(WalletServiceError::MissingDeclaration(
                active_op.declaration_id,
            ))?;

        let zk_sig = Self::sign_zksig(tx_hash, [declaration.zk_id], kms).await?;

        Ok(OpProof::ZkSig(zk_sig))
    }

    async fn sign_leader_claim(
        tx_hash: TxHash,
        leader_claim_op: &LeaderClaimOp,
        tip: HeaderId,
        wallet: &Wallet,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<OpProof, WalletServiceError> {
        let (voucher_master_key_id, voucher_index) = wallet
            .get_voucher_by_nullifier(&leader_claim_op.voucher_nullifier)
            .ok_or(WalletServiceError::VoucherNotFound(
                leader_claim_op.voucher_nullifier,
            ))?;
        let voucher_secret =
            Self::derive_voucher_from_kms(kms, voucher_master_key_id.clone(), *voucher_index).await;

        let voucher_cm = VoucherCm::from_secret(voucher_secret);
        let path = wallet
            .voucher_path_snapshot(tip, &voucher_cm)
            .map_err(WalletServiceError::WalletError)?
            .ok_or(WalletServiceError::VoucherMerklePathNotFound(voucher_cm))?;
        let rewards_root = leader_claim_op.rewards_root;

        // TODO: This should happen in KMS
        let poc = tokio::task::spawn_blocking(move || {
            Self::generate_poc(voucher_secret, &path, rewards_root, tx_hash)
        })
        .await??;

        Ok(OpProof::PoC(poc))
    }

    async fn sign_transfer(
        tx_hash: TxHash,
        note_ids: Vec<NoteId>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        ledger: &LedgerState,
    ) -> Result<OpProof, WalletServiceError> {
        let input_pks = Self::resolve_note_input_pks(ledger, note_ids)?;
        let zk_sig = Self::sign_zksig(tx_hash, input_pks, kms).await?;

        Ok(OpProof::ZkSig(zk_sig))
    }

    async fn sign_tx(
        tx_builder: MantleTxBuilder,
        tip: HeaderId,
        tip_leader: LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        wallet: &Wallet,
    ) -> Result<SignedMantleTx, WalletServiceError> {
        // Extract input public keys before building the transaction
        let mut channel_withdraw_proofs = tx_builder.channel_withdraw_proofs().clone();
        let mantle_tx = tx_builder.clone().build();
        let tx_hash = mantle_tx.hash();

        let mut ops_proofs = Vec::new();
        for (i, op) in mantle_tx.ops().iter().enumerate() {
            let proof = match op {
                Op::ChannelInscribe(inscribe_op) => {
                    Self::sign_inscription(tx_hash, inscribe_op, kms).await?
                }
                Op::ChannelSetKeys(set_keys_op) => {
                    Self::sign_channel_set_key(tx_hash, set_keys_op, &tip_leader, kms).await?
                }
                Op::ChannelDeposit(deposit_op) => {
                    Self::sign_channel_deposit(
                        tx_hash,
                        deposit_op.inputs.as_ref().clone(),
                        kms,
                        &tip_leader,
                    )
                    .await?
                }
                Op::ChannelWithdraw(_channel_withdraw_op) => {
                    let proof = channel_withdraw_proofs
                        .remove(&i)
                        .ok_or(WalletServiceError::ChannelWithdrawProofNotFound(i))?;
                    OpProof::ChannelWithdrawProof(proof)
                }
                Op::SDPDeclare(declare_op) => {
                    Self::sign_sdp_declare(tx_hash, declare_op, &tip_leader, kms).await?
                }
                Op::SDPWithdraw(withdraw_op) => {
                    Self::sign_sdp_withdraw(tx_hash, withdraw_op, &tip_leader, kms).await?
                }
                Op::SDPActive(active_op) => {
                    Self::sign_sdp_active(tx_hash, active_op, &tip_leader, kms).await?
                }
                Op::LeaderClaim(claim_op) => {
                    Self::sign_leader_claim(tx_hash, claim_op, tip, wallet, kms).await?
                }
                Op::Transfer(transfer_op) => {
                    Self::sign_transfer(
                        tx_hash,
                        transfer_op.inputs.as_ref().clone(),
                        kms,
                        &tip_leader,
                    )
                    .await?
                }
            };
            ops_proofs.push(proof);
        }

        let signed_mantle_tx = SignedMantleTx::new(mantle_tx, ops_proofs)
            .expect("Failed to create signed transaction");

        Ok(signed_mantle_tx)
    }

    async fn sign_ed25519(
        tx_hash: TxHash,
        pk: <Ed25519Key as SecuredKey>::PublicKey,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<<Ed25519Key as SecuredKey>::Signature, WalletServiceError> {
        // Use hex-encoded public key as key_id for now
        let key_id = hex::encode(pk.as_bytes());

        let payload = PayloadEncoding::Ed25519(tx_hash.as_signing_bytes());
        let signature = kms
            .sign(key_id, payload)
            .await
            .map_err(WalletServiceError::KmsApi)?;

        let SignatureEncoding::Ed25519(ed25519_sig) = signature else {
            return Err(WalletServiceError::KmsApi(
                "Expected Ed25519 signature".into(),
            ));
        };

        Ok(ed25519_sig)
    }

    async fn sign_zksig(
        tx_hash: TxHash,
        pks: impl IntoIterator<Item = ZkPublicKey>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<ZkSignature, WalletServiceError> {
        // Use hex-encoded public key as key_id for now
        let key_ids: Vec<_> = pks
            .into_iter()
            .map(|pk| hex::encode(lb_groth16::fr_to_bytes(&pk.into_inner())))
            .collect();

        let payload = PayloadEncoding::Zk(tx_hash.to_fr());
        let signature = kms
            .sign_multiple(key_ids, payload)
            .await
            .map_err(WalletServiceError::KmsApi)?;

        let SignatureEncoding::Zk(zk_sig) = signature else {
            return Err(WalletServiceError::KmsApi(
                "Expected ZkSig signature".into(),
            ));
        };

        Ok(zk_sig)
    }

    fn resolve_note_input_pks(
        ledger: &LedgerState,
        note_ids: impl IntoIterator<Item = NoteId>,
    ) -> Result<Vec<ZkPublicKey>, WalletServiceError> {
        note_ids
            .into_iter()
            .map(|note_id| {
                ledger
                    .latest_utxos()
                    .utxos()
                    .get(&note_id)
                    .map(|(utxo, _)| utxo.note.pk)
                    .ok_or(WalletServiceError::MissingInputNote(note_id))
            })
            .collect()
    }

    fn generate_poc(
        voucher_secret: VoucherSecret,
        path: &MerklePath,
        rewards_root: RewardsRoot,
        tx_hash: TxHash,
    ) -> Result<Groth16LeaderClaimProof, WalletServiceError> {
        Ok(Groth16LeaderClaimProof::prove(LeaderClaimPrivate::new(
            LeaderClaimPublic {
                voucher_root: rewards_root.into(),
                mantle_tx_hash: tx_hash.to_fr(),
            },
            path,
            voucher_secret,
        ))?)
    }

    async fn get_leader_aged_notes(
        tip: Option<HeaderId>,
        resp_tx: Sender<Result<TipResponse<Vec<UtxoWithKeyId>>, WalletServiceError>>,
        wallet: &Wallet,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    ) {
        let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
            Ok(tip) => tip,
            Err(err) => {
                Self::send_err(resp_tx, err);
                return;
            }
        };

        // Get the ledger state at the specified tip
        let Ok(Some(ledger_state)) = cryptarchia.get_ledger_state(tip).await else {
            Self::send_err(resp_tx, WalletServiceError::LedgerStateNotFound(tip));
            return;
        };

        let wallet_state = match wallet.wallet_state_at(tip) {
            Ok(wallet_state) => wallet_state,
            Err(err) => {
                error!(err = ?err, "Failed to fetch wallet state");
                Self::send_err(
                    resp_tx,
                    WalletServiceError::FailedToFetchWalletStateForBlock(tip),
                );
                return;
            }
        };

        let aged_utxos = ledger_state.epoch_state().utxos.utxos();
        let eligible_utxos = wallet_state
            .utxos
            .iter()
            .filter(|(note_id, _)| aged_utxos.contains_key(note_id))
            .filter_map(|(_, utxo)| {
                wallet
                    .known_keys()
                    .get(&utxo.note.pk)
                    .map(|key_id| UtxoWithKeyId {
                        utxo: *utxo,
                        key_id: key_id.clone(),
                    })
            })
            .collect();

        if resp_tx
            .send(Ok(TipResponse {
                tip,
                response: eligible_utxos,
            }))
            .is_err()
        {
            error!("Failed to respond to GetLeaderAgedNotes");
        }
    }

    /// Derive a new voucher via KMS and store it in [`Wallet`].
    async fn generate_new_voucher_secret(
        state: &mut ServiceState<'_>,
        master_key_id: KeyId,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        resp_tx: Sender<VoucherCm>,
    ) {
        let index = state.get_and_inc_next_new_voucher_index();
        let secret = Self::derive_voucher_from_kms(kms, master_key_id.clone(), index).await;
        let cm = VoucherCm::from_secret(secret);
        let nf = VoucherNullifier::from_secret(secret);

        state.add_known_voucher(cm, nf, (master_key_id, index));

        if let Err(e) = resp_tx.send(cm) {
            error!("Failed to send voucher secret: {e:?}");
        }
    }

    /// Derive voucher secret from KMS given master key and index.
    // TODO: Use secure KMS operator that returns `VoucherCm` and `VoucherNullifier`
    async fn derive_voucher_from_kms(
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        key_id: KeyId,
        index: u64,
    ) -> VoucherSecret {
        let (output_tx, output_rx) = oneshot::channel();
        let () = kms
            .execute(
                key_id,
                KeyOperators::Zk(Box::new(UnsafeVoucherOperator::new(
                    index.into(),
                    output_tx,
                ))),
            )
            .await
            .expect("KMS API should be invoked");
        output_rx
            .await
            .expect("KMS API should respond with voucher_cm")
            .into()
    }

    async fn get_claimable_voucher(
        tip: Option<HeaderId>,
        resp_tx: Sender<
            Result<TipResponse<Option<VoucherCommitmentAndNullifier>>, WalletServiceError>,
        >,
        wallet: &Wallet,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    ) {
        let tip = match Self::msg_tip_or_latest(tip, cryptarchia).await {
            Ok(tip) => tip,
            Err(err) => {
                Self::send_err(resp_tx, err);
                return;
            }
        };

        let voucher = Self::find_claimable_voucher(wallet, tip);
        if resp_tx
            .send(Ok(TipResponse {
                tip,
                response: voucher,
            }))
            .is_err()
        {
            error!("Failed to respond to GetClaimableVoucher");
        }
    }

    fn find_claimable_voucher(
        wallet: &Wallet,
        tip: HeaderId,
    ) -> Option<VoucherCommitmentAndNullifier> {
        for (nf, cm) in wallet.voucher_commitments_and_nullifiers() {
            if let Ok(Some(_)) = wallet.voucher_path_snapshot(tip, cm) {
                return Some(VoucherCommitmentAndNullifier {
                    commitment: *cm,
                    nullifier: *nf,
                });
            }
        }
        None
    }

    async fn backfill_if_not_in_sync(
        tip: Option<HeaderId>,
        state: &mut ServiceState<'_>,
        storage: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
        epoch_config: &EpochConfig,
    ) -> Result<(), WalletServiceError> {
        let tip = Self::msg_tip_or_latest(tip, cryptarchia).await?;

        if state.wallet().has_processed_block(tip) {
            // We are already in sync with `tip`.
            return Ok(());
        }

        // The caller knows a more recent tip than the wallet.
        // To resolve this, we do a JIT backfill to try to sync the wallet with
        // cryptarchia. If we still have not caught up after the backfill, we return an
        // error to the caller
        Self::backfill_missing_blocks(tip, state, storage, cryptarchia, epoch_config).await?;

        if state.wallet().has_processed_block(tip) {
            Ok(())
        } else {
            error!("Failed to backfill wallet to {tip}");
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
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        cryptarchia_api: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
        epoch_config: &EpochConfig,
    ) {
        let Ok(block) = Self::load_block(
            header_id,
            storage_adapter,
        )
            .await
            .inspect_err(|e| {
                error!(block_id=?header_id, err=%e, "Failed to fetch new block and ledger for wallet");
            }) else {
            return;
        };

        let wallet_block =
            WalletBlock::from_block(&block, epoch_config.epoch(block.header().slot()));
        match state.apply_block(&wallet_block) {
            Ok(()) => {
                trace!(block_id=?wallet_block.id, "Applied block to wallet");
            }
            Err(WalletError::UnknownBlock(block_id)) => {
                debug!(block_id = ?block_id, "Missing block in wallet, backfilling");
                if let Err(e) = Self::backfill_missing_blocks(
                    wallet_block.id,
                    state,
                    storage_adapter,
                    cryptarchia_api,
                    epoch_config,
                )
                .await
                {
                    error!(block_id=?header_id, err=%e, "Failed to backfill missing block to wallet");
                }
            }
            Err(e) => {
                error!(err=%e, "unexexpected error while applying block to wallet");
            }
        }
    }

    async fn load_block(
        header_id: HeaderId,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> Result<Block<Tx>, WalletServiceError> {
        storage_adapter
            .get_block(&header_id)
            .await
            .ok_or(WalletServiceError::BlockNotFoundInStorage(header_id))
    }

    async fn handle_lib_update(
        lib_update: &LibUpdate,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        state: &mut ServiceState<'_>,
    ) {
        debug!(
            new_lib = ?lib_update.new_lib,
            stale_blocks_count = lib_update.pruned_blocks.stale_blocks.len(),
            immutable_blocks_count = lib_update.pruned_blocks.immutable_blocks.len(),
            "Received LIB update"
        );

        state.advance_lib(
            lib_update.new_lib,
            lib_update.pruned_blocks.all(),
            Self::collect_claimed_nullifiers_from_blocks(
                lib_update.pruned_blocks.immutable_blocks.values(),
                storage_adapter,
            )
            .await,
        );
    }

    async fn collect_claimed_nullifiers_from_blocks(
        blocks: impl Iterator<Item = &HeaderId>,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> impl IntoIterator<Item = VoucherNullifier> {
        let immutable_blocks: Vec<Block<Tx>> = futures::stream::iter(blocks)
            .filter_map(async |header_id| storage_adapter.get_block(header_id).await)
            .collect::<Vec<_>>()
            .await;
        let claimed_nullifiers: Vec<VoucherNullifier> = immutable_blocks
            .into_iter()
            .flat_map(|block: Block<Tx>| block.into_transactions().into_iter())
            .flat_map(|tx: Tx| {
                tx.ops_with_proof()
                    .map(|(op, _)| op.clone())
                    .collect::<Vec<_>>()
            })
            .filter_map(|op| {
                if let Op::LeaderClaim(claim_op) = op {
                    Some(claim_op.voucher_nullifier)
                } else {
                    None
                }
            })
            .collect();
        claimed_nullifiers.into_iter()
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn backfill_missing_blocks(
        tip: HeaderId,
        state: &mut ServiceState<'_>,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        cryptarchia_api: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
        epoch_config: &EpochConfig,
    ) -> Result<(), WalletServiceError> {
        debug!(from_tip = ?tip, to_state_lib = ?state.lib(), "backfilling missing blocks");

        // Fetch block IDs in [state.lib, tip]
        let missing_headers = cryptarchia_api
            .get_headers(tip, state.lib())
            .await
            .map_err(WalletServiceError::CryptarchiaApi)
            .inspect_err(|e| {
                error!(block_id = ?tip, err = %e, "Failed to fetch missing headers for backfill");
            })?
            .try_collect::<Vec<_>>()
            .await?;

        if !missing_headers.is_empty() {
            debug!(
                "Backfilling wallet to tip {tip:?} with {} missing headers",
                missing_headers.len()
            );
        }

        // Load/apply blocks in order from `state.lib` to `tip`
        for header_id in missing_headers.into_iter().rev() {
            if state.wallet().has_processed_block(header_id) {
                debug!("Skipping already processed wallet block {header_id:?}");
                continue;
            }

            let block = Self::load_block(header_id, storage_adapter).await?;
            let wallet_block =
                WalletBlock::from_block(&block, epoch_config.epoch(block.header().slot()));

            if let Err(e) = state.apply_block(&wallet_block) {
                error!(
                    block_id = ?header_id,
                    err = %e,
                    "Failed to apply backfill block to wallet"
                );
                return Err(WalletServiceError::FailedToApplyBlock(header_id));
            }
        }

        Ok(())
    }

    fn send_err<T: std::fmt::Debug>(
        tx: Sender<Result<T, WalletServiceError>>,
        err: WalletServiceError,
    ) {
        if let Err(msg) = tx.send(Err(err)) {
            error!(msg = ?msg, "Wallet failed to send error response");
        }
    }

    fn get_known_addresses(
        wallet: &Wallet,
        tx: Sender<Result<Vec<ZkPublicKey>, WalletServiceError>>,
    ) {
        let response: Vec<_> = wallet.known_keys().keys().copied().collect();
        if let Err(e) = tx.send(Ok(response)) {
            error!(err = ?e, "Failed to send known addresses response");
        }
    }

    async fn get_tx_context(
        block_id: Option<HeaderId>,
        resp_tx: Sender<Result<MantleTxContext, WalletServiceError>>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    ) {
        let block_id = match Self::msg_tip_or_latest(block_id, cryptarchia).await {
            Ok(block_id) => block_id,
            Err(error) => {
                Self::send_err(resp_tx, error);
                return;
            }
        };

        let ledger_state = match cryptarchia.get_ledger_state(block_id).await {
            Ok(Some(ledger_state)) => ledger_state,
            Ok(None) => {
                Self::send_err(resp_tx, WalletServiceError::LedgerStateNotFound(block_id));
                return;
            }
            Err(err) => {
                Self::send_err(resp_tx, WalletServiceError::from(err));
                return;
            }
        };

        if let Err(e) = resp_tx.send(Ok(ledger_state.tx_context())) {
            error!(err = ?e, "Failed to send gas context response");
        }
    }
}

/// A config to calculate epoch from slot
struct EpochConfig {
    epoch_config: lb_cryptarchia_engine::EpochConfig,
    consensus_config: lb_cryptarchia_engine::Config,
}

impl EpochConfig {
    fn epoch(&self, slot: Slot) -> Epoch {
        self.epoch_config
            .epoch(slot, self.consensus_config.base_period_length())
    }
}
