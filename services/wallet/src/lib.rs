pub mod api;
mod states;

use std::{collections::HashMap, path::PathBuf, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt as _;
use lb_chain_service::{
    LibUpdate,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
    storage::{StorageAdapter as _, adapters::StorageAdapter},
};
use lb_core::{
    block::Block,
    header::HeaderId,
    mantle::{
        AuthenticatedMantleTx, Op, OpProof, SignedMantleTx, Transaction as _, TxHash, Utxo, Value,
        gas::MainnetGasConstants,
        ops::{
            channel::ChannelId,
            leader_claim::{
                LeaderClaimOp, RewardsRoot, VoucherCm, VoucherNullifier, VoucherSecret,
            },
        },
        tx_builder::MantleTxBuilder,
    },
    proofs::leader_claim_proof::{Groth16LeaderClaimProof, LeaderClaimPrivate, LeaderClaimPublic},
};
use lb_groth16::Fr;
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
use lb_services_utils::{
    overwatch::{JsonFileBackend, RecoveryOperator, recovery::backends::FileBackendSettings},
    wait_until_services_are_ready,
};
use lb_storage_service::{api::chain::StorageChainApi, backends::StorageBackend};
use lb_utxotree::MerklePath;
use lb_wallet::{WalletBlock, WalletError};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    sync::{oneshot, oneshot::Sender},
    task::JoinError,
};
use tracing::{debug, error, info, trace};

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
    MissingLockedNote(lb_core::mantle::NoteId),

    #[error("PoC generation failed: {0:?}")]
    PoCGenerationFailed(#[from] lb_core::proofs::leader_claim_proof::Error),

    #[error("Voucher not found for the nullifier")]
    VoucherNotFound(VoucherNullifier),

    #[error("Merkle path not found for voucher_cm: {0:?}")]
    VoucherMerklePathNotFound(VoucherCm),

    #[error("blocking task failed: {0}")]
    TaskJoin(#[from] JoinError),
}

#[derive(Debug)]
pub enum WalletMsg {
    GetBalance {
        tip: Option<HeaderId>,
        pk: ZkPublicKey,
        resp_tx: Sender<Result<TipResponse<Option<Value>>, WalletServiceError>>,
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
            | Self::GetClaimableVoucher { tip, .. } => *tip,
            Self::GenerateNewVoucherSecret { .. } | Self::GetKnownAddresses { .. } => None,
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

        wait_until_services_are_ready!(
            &service_resources_handle.overwatch_handle,
            Some(Duration::from_secs(60)),
            lb_storage_service::StorageService<_, _>,
            Cryptarchia,
            Kms
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
        )
        .await?;

        service_resources_handle.status_updater.notify_ready();
        info!("Wallet service is ready and subscribed to blocks");

        loop {
            tokio::select! {
                Some(msg) = service_resources_handle.inbound_relay.recv() => {
                    Self::handle_wallet_message(msg, &mut state, &voucher_master_key_id, &storage_adapter, &cryptarchia_api, &kms).await;
                }
                Ok(event) = new_block_receiver.recv() => {
                    Self::handle_new_block(event.block_id, &mut state, &storage_adapter, &cryptarchia_api).await;
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

    async fn handle_wallet_message(
        msg: WalletMsg,
        state: &mut ServiceState<'_>,
        voucher_master_key_id: &KeyId,
        storage: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) {
        if let Err(err) =
            Self::backfill_if_not_in_sync(msg.tip(), state, storage, cryptarchia).await
        {
            error!(err=?err, "Failed backfilling wallet to message tip, will attempt to continue processing the message {msg:?}");
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

                let resp = Self::sign_tx(tx_builder, ledger, kms, state.wallet())
                    .await
                    .map(|signed_tx| TipResponse {
                        tip,
                        response: signed_tx,
                    });

                if resp_tx.send(resp).is_err() {
                    error!("Failed to respond to SignTx");
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
        }
    }

    async fn handle_get_balance(
        tip: Option<HeaderId>,
        pk: ZkPublicKey,
        resp_tx: Sender<Result<TipResponse<Option<u64>>, WalletServiceError>>,
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

    async fn sign_tx(
        tx_builder: MantleTxBuilder,
        ledger: LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
        wallet: &Wallet,
    ) -> Result<SignedMantleTx, WalletServiceError> {
        // Extract input public keys before building the transaction
        let input_pks: Vec<ZkPublicKey> = tx_builder
            .ledger_inputs()
            .iter()
            .map(|utxo| utxo.note.pk)
            .collect();

        let mantle_tx = tx_builder.build();
        let tx_hash = mantle_tx.hash();

        let mut ops_proofs = Vec::new();
        for op in &mantle_tx.ops {
            let proof = match op {
                Op::ChannelInscribe(inscribe_op) => {
                    let ed25519_sig = Self::sign_ed25519(tx_hash, inscribe_op.signer, kms).await?;
                    OpProof::Ed25519Sig(ed25519_sig)
                }
                Op::ChannelSetKeys(set_keys_op) => {
                    let channel = ledger
                        .mantle_ledger()
                        .channels()
                        .channel_state(&set_keys_op.channel)
                        .ok_or(WalletServiceError::MissingChannelState(set_keys_op.channel))?;

                    let authorized_key = channel.keys[0]; // First key is authorized key (guaranteed non-empty)
                    let ed25519_sig = Self::sign_ed25519(tx_hash, authorized_key, kms).await?;

                    OpProof::Ed25519Sig(ed25519_sig)
                }
                Op::SDPDeclare(declare_op) => {
                    // For a new declaration, the note is still in the UTXOs (not yet locked).
                    // We look it up from the UTXO set to get the public key for signing.
                    let utxo_tree = ledger.latest_utxos();
                    info!(
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

                    let zk_sig =
                        Self::sign_zksig(tx_hash, [note.pk, declare_op.zk_id], kms).await?;
                    let ed25519_sig =
                        Self::sign_ed25519(tx_hash, declare_op.provider_id.0, kms).await?;

                    OpProof::ZkAndEd25519Sigs {
                        zk_sig,
                        ed25519_sig,
                    }
                }
                Op::SDPWithdraw(withdraw_op) => {
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

                    let zk_sig =
                        Self::sign_zksig(tx_hash, [locked_note.pk, declaration.zk_id], kms).await?;

                    OpProof::ZkSig(zk_sig)
                }
                Op::SDPActive(active_op) => {
                    let declaration = ledger
                        .mantle_ledger()
                        .sdp_ledger()
                        .get_declaration(&active_op.declaration_id)
                        .ok_or(WalletServiceError::MissingDeclaration(
                            active_op.declaration_id,
                        ))?;

                    let locked_note = ledger
                        .mantle_ledger()
                        .locked_notes()
                        .get(&declaration.locked_note_id)
                        .ok_or(WalletServiceError::MissingLockedNote(
                            declaration.locked_note_id,
                        ))?;

                    let zk_sig =
                        Self::sign_zksig(tx_hash, [locked_note.pk, declaration.zk_id], kms).await?;

                    OpProof::ZkSig(zk_sig)
                }
                Op::LeaderClaim(claim_op) => {
                    Self::prove_leader_claim_op(claim_op.clone(), tx_hash, &ledger, wallet, kms)
                        .await?
                }
            };
            ops_proofs.push(proof);
        }

        let ledger_tx_proof = Self::sign_zksig(tx_hash, input_pks, kms).await?;

        let signed_mantle_tx = SignedMantleTx::new(mantle_tx, ops_proofs, ledger_tx_proof)
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

        let payload = PayloadEncoding::Zk(tx_hash.into());
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

    async fn prove_leader_claim_op(
        op: LeaderClaimOp,
        tx_hash: TxHash,
        ledger: &LedgerState,
        wallet: &Wallet,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) -> Result<OpProof, WalletServiceError> {
        let (voucher_master_key_id, voucher_index) = wallet
            .get_voucher_by_nullifier(&op.voucher_nullifier)
            .ok_or(WalletServiceError::VoucherNotFound(op.voucher_nullifier))?;
        let voucher_secret =
            Self::derive_voucher_from_kms(kms, voucher_master_key_id.clone(), *voucher_index).await;

        let voucher_cm = VoucherCm::from_secret(voucher_secret);
        let path = ledger
            .mantle_ledger()
            .voucher_merkle_path(voucher_cm)
            .ok_or(WalletServiceError::VoucherMerklePathNotFound(voucher_cm))?;

        // TODO: This should happen in KMS
        let poc = tokio::task::spawn_blocking(move || {
            Self::generate_poc(voucher_secret, &path, op.rewards_root, tx_hash)
        })
        .await??;

        Ok(OpProof::PoC(poc))
    }

    fn generate_poc(
        voucher_secret: VoucherSecret,
        path: &MerklePath<Fr>,
        rewards_root: RewardsRoot,
        tx_hash: TxHash,
    ) -> Result<Groth16LeaderClaimProof, WalletServiceError> {
        Ok(Groth16LeaderClaimProof::prove(LeaderClaimPrivate::new(
            LeaderClaimPublic {
                voucher_root: rewards_root.into(),
                mantle_tx_hash: tx_hash.into(),
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

        // Get the ledger state at the specified tip
        let Ok(Some(ledger_state)) = cryptarchia.get_ledger_state(tip).await else {
            Self::send_err(resp_tx, WalletServiceError::LedgerStateNotFound(tip));
            return;
        };

        let voucher = Self::find_claimable_voucher(wallet, &ledger_state);
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
        ledger_state: &LedgerState,
    ) -> Option<VoucherCommitmentAndNullifier> {
        for (nf, cm) in wallet.voucher_commitments_and_nullifiers() {
            if ledger_state.mantle_ledger().has_claimable_voucher(cm) {
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
        Self::backfill_missing_blocks(tip, state, storage, cryptarchia).await?;

        if state.wallet().has_processed_block(tip) {
            Ok(())
        } else {
            error!("Failed to backfill wallet to {tip}");
            Err(WalletServiceError::FailedToFetchWalletStateForBlock(tip))
        }
    }

    async fn handle_new_block(
        header_id: HeaderId,
        state: &mut ServiceState<'_>,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        cryptarchia_api: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
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

        let wallet_block = WalletBlock::from(block);
        match state.apply_block(&wallet_block) {
            Ok(()) => {
                trace!(block_id=?wallet_block.id, "Applied block to wallet");
            }
            Err(WalletError::UnknownBlock(block_id)) => {
                info!(block_id = ?block_id, "Missing block in wallet, backfilling");
                if let Err(e) = Self::backfill_missing_blocks(
                    wallet_block.id,
                    state,
                    storage_adapter,
                    cryptarchia_api,
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

        state.prune_states(lib_update.pruned_blocks.all());
        let immutable_blocks: Vec<Block<Tx>> =
            futures::stream::iter(lib_update.pruned_blocks.immutable_blocks.values())
                .filter_map(async |header_id: &HeaderId| storage_adapter.get_block(header_id).await)
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
        state.prune_vouchers(claimed_nullifiers);
    }

    async fn backfill_missing_blocks(
        tip: HeaderId,
        state: &mut ServiceState<'_>,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        cryptarchia_api: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    ) -> Result<(), WalletServiceError> {
        let missing_headers = cryptarchia_api
            .get_headers_to_lib(tip)
            .await
            .map_err(WalletServiceError::CryptarchiaApi)
            .inspect_err(|e| {
                error!(block_id = ?tip, err = %e, "Failed to fetch missing headers for backfill");
            })?;

        for header_id in missing_headers.iter().rev().copied() {
            if state.wallet().has_processed_block(header_id) {
                info!("skipping already processed block");
                continue;
            }

            let block = Self::load_block(header_id, storage_adapter).await?;

            if let Err(e) = state.apply_block(&block.into()) {
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
}
