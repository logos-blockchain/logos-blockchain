pub mod api;

use std::{collections::HashSet, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use chain_service::{
    LibUpdate,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
    storage::{StorageAdapter as _, adapters::storage::StorageAdapter},
};
use groth16::fr_to_bytes;
use key_management_system_service::{
    api::{KmsServiceApi, KmsServiceData},
    backend::preload::PreloadKMSBackend,
    keys::{
        Ed25519Key, PayloadEncoding, SignatureEncoding, ZkPublicKey, ZkSignature,
        secured_key::SecuredKey,
    },
};
use nomos_core::{
    block::Block,
    header::HeaderId,
    mantle::{
        AuthenticatedMantleTx, SignedMantleTx, Transaction as _, TxHash, Utxo, Value,
        gas::MainnetGasConstants,
        ops::{Op, OpProof, channel::ChannelId},
        tx_builder::MantleTxBuilder,
    },
};
use nomos_ledger::LedgerState;
use nomos_storage::{api::chain::StorageChainApi, backends::StorageBackend};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Serialize, de::DeserializeOwned};
use services_utils::wait_until_services_are_ready;
use tokio::sync::oneshot;
use tracing::{debug, error, info, trace};
use wallet::{Wallet, WalletBlock, WalletError};

#[derive(Debug, thiserror::Error)]
pub enum WalletServiceError {
    #[error("Ledger state corresponding to block {0} not found")]
    LedgerStateNotFound(HeaderId),

    #[error("Wallet state corresponding to block {0} not found")]
    FailedToFetchWalletStateForBlock(HeaderId),

    #[error("Failed to apply historical block {0} to wallet")]
    FailedToApplyBlock(HeaderId),

    #[error("Block {0} not found in storage during wallet sync")]
    BlockNotFoundInStorage(HeaderId),

    #[error(transparent)]
    WalletError(#[from] WalletError),

    #[error("KMS API error: {0}")]
    KmsApi(DynError),

    #[error("Cryptarchia API error: {0}")]
    CryptarchiaApi(#[from] chain_service::api::ApiError),

    #[error("Channel {0:?} is missing state in ledger")]
    MissingChannelState(ChannelId),

    #[error("Declaration {0:?} is missing in ledger")]
    MissingDeclaration(nomos_core::sdp::DeclarationId),

    #[error("Locked note {0:?} is missing in ledger")]
    MissingLockedNote(nomos_core::mantle::NoteId),
}

#[derive(Debug)]
pub enum WalletMsg {
    GetBalance {
        tip: HeaderId,
        pk: ZkPublicKey,
        resp_tx: oneshot::Sender<Result<Option<Value>, WalletServiceError>>,
    },
    FundAndSignTx {
        tip: HeaderId,
        tx_builder: MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        resp_tx: oneshot::Sender<Result<SignedMantleTx, WalletServiceError>>,
    },
    GetLeaderAgedNotes {
        tip: HeaderId,
        resp_tx: oneshot::Sender<Result<Vec<Utxo>, WalletServiceError>>,
    },
}

impl WalletMsg {
    #[must_use]
    pub const fn tip(&self) -> HeaderId {
        match self {
            Self::GetBalance { tip, .. }
            | Self::FundAndSignTx { tip, .. }
            | Self::GetLeaderAgedNotes { tip, .. } => *tip,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WalletServiceSettings {
    pub known_keys: HashSet<ZkPublicKey>,
}

pub struct WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    _marker: std::marker::PhantomData<(Kms, Cryptarchia, Tx, Storage)>,
}

impl<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId> ServiceData
    for WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
{
    type Settings = WalletServiceSettings;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = WalletMsg;
}

#[async_trait]
impl<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
where
    Kms: KmsServiceData<Backend = PreloadKMSBackend> + Send + Sync,
    Tx: AuthenticatedMantleTx + Send + Sync + Clone + Eq + Serialize + DeserializeOwned + 'static,
    Cryptarchia: CryptarchiaServiceData<Tx = Tx>,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>>,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<Cryptarchia>
        + AsServiceId<nomos_storage::StorageService<Storage, RuntimeServiceId>>
        + AsServiceId<Kms>
        + std::fmt::Debug
        + std::fmt::Display
        + Send
        + Sync
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            service_resources_handle,
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
            nomos_storage::StorageService<_, _>,
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
            .relay::<nomos_storage::StorageService<Storage, RuntimeServiceId>>()
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

        let mut wallet = Wallet::from_lib(settings.known_keys.clone(), lib, &lib_ledger);

        Self::backfill_missing_blocks(
            &Self::fetch_missing_headers(chain_info.tip, &cryptarchia_api).await?,
            &mut wallet,
            &storage_adapter,
        )
        .await?;

        service_resources_handle.status_updater.notify_ready();
        info!("Wallet service is ready and subscribed to blocks");

        loop {
            tokio::select! {
                Some(msg) = service_resources_handle.inbound_relay.recv() => {
                    Self::handle_wallet_message(msg, &mut wallet, &storage_adapter, &cryptarchia_api, &kms).await;
                }

                Ok(header_id) = new_block_receiver.recv() => {
                    let Some(block) = storage_adapter.get_block(&header_id).await else {
                        error!(block_id=?header_id, "Missing block in storage");
                        continue;
                    };
                    let wallet_block = WalletBlock::from(block);
                    match wallet.apply_block(&wallet_block) {
                        Ok(()) => {
                            trace!(block_id = ?wallet_block.id, "Applied block to wallet");
                        }
                        Err(WalletError::UnknownBlock(block_id)) => {

                            info!(block_id = ?block_id, "Missing block in wallet, backfilling");
                            Self::backfill_missing_blocks(&Self::fetch_missing_headers(wallet_block.id, &cryptarchia_api).await?, &mut wallet, &storage_adapter).await?;
                        },
                        Err(err) => {
                            error!(err=?err, "unexexpected error while applying block to wallet");
                        }
                    }
                }

                Ok(lib_update) = lib_receiver.recv() => {
                    Self::handle_lib_update(&lib_update, &mut wallet);
                }
            }
        }
    }
}

impl<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
    WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
where
    Kms: KmsServiceData<Backend = PreloadKMSBackend>,
    Tx: AuthenticatedMantleTx + Send + Sync + Clone + Eq + Serialize + DeserializeOwned + 'static,
    Cryptarchia: CryptarchiaServiceData<Tx = Tx> + Send + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>>,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    RuntimeServiceId:
        AsServiceId<Cryptarchia> + AsServiceId<Kms> + std::fmt::Debug + std::fmt::Display + Sync,
{
    async fn handle_wallet_message(
        msg: WalletMsg,
        wallet: &mut Wallet,
        storage: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) {
        if let Err(err) =
            Self::backfill_if_not_in_sync(msg.tip(), wallet, storage, cryptarchia).await
        {
            error!(err=?err, "Failed backfilling wallet to message tip, will attempt to continue processing the message {msg:?}");
        }

        match msg {
            WalletMsg::GetBalance { tip, pk, resp_tx } => {
                Self::handle_get_balance(tip, pk, resp_tx, wallet);
            }
            WalletMsg::FundAndSignTx {
                tip,
                tx_builder,
                change_pk,
                funding_pks,
                resp_tx,
            } => {
                let funded = match wallet.fund_tx::<MainnetGasConstants>(
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

                Self::handle_sign_tx(funded, ledger, resp_tx, kms).await;
            }
            WalletMsg::GetLeaderAgedNotes { tip, resp_tx } => {
                Self::get_leader_aged_notes(tip, resp_tx, wallet, cryptarchia).await;
            }
        }
    }

    fn handle_get_balance(
        tip: HeaderId,
        pk: ZkPublicKey,
        resp_tx: oneshot::Sender<Result<Option<u64>, WalletServiceError>>,
        wallet: &Wallet,
    ) {
        let balance = wallet
            .balance(tip, pk)
            .map_err(WalletServiceError::WalletError);

        if resp_tx.send(balance).is_err() {
            error!("Failed to respond to GetBalance");
        }
    }

    async fn handle_sign_tx(
        tx_builder: MantleTxBuilder,
        ledger: LedgerState,
        resp_tx: oneshot::Sender<Result<SignedMantleTx, WalletServiceError>>,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
    ) {
        let signed_tx_res = Self::sign_tx(tx_builder, ledger, kms).await;

        if resp_tx.send(signed_tx_res).is_err() {
            error!("Failed to respond to FundAndSignTx");
        }
    }

    async fn sign_tx(
        tx_builder: MantleTxBuilder,
        ledger: LedgerState,
        kms: &KmsServiceApi<Kms, RuntimeServiceId>,
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
                Op::ChannelBlob(blob_op) => {
                    let ed25519_sig = Self::sign_ed25519(tx_hash, blob_op.signer, kms).await?;
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
                    let locked_note = ledger
                        .mantle_ledger()
                        .locked_notes()
                        .get(&declare_op.locked_note_id)
                        .ok_or(WalletServiceError::MissingLockedNote(
                            declare_op.locked_note_id,
                        ))?;

                    let zk_sig =
                        Self::sign_zksig(tx_hash, [locked_note.pk, declare_op.zk_id], kms).await?;
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

                    let zk_sig = Self::sign_zksig(tx_hash, [declaration.zk_id], kms).await?;

                    OpProof::ZkSig(zk_sig)
                }
                Op::LeaderClaim(_claim_op) => {
                    todo!("LeaderClaim proof not yet implemented")
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
            .map(|pk| hex::encode(fr_to_bytes(&pk.into_inner())))
            .collect();

        let payload = PayloadEncoding::Ed25519(tx_hash.as_signing_bytes());
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

    async fn get_leader_aged_notes(
        tip: HeaderId,
        tx: oneshot::Sender<Result<Vec<Utxo>, WalletServiceError>>,
        wallet: &Wallet,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    ) {
        // Get the ledger state at the specified tip
        let Ok(Some(ledger_state)) = cryptarchia.get_ledger_state(tip).await else {
            Self::send_err(tx, WalletServiceError::LedgerStateNotFound(tip));
            return;
        };

        let wallet_state = match wallet.wallet_state_at(tip) {
            Ok(wallet_state) => wallet_state,
            Err(err) => {
                error!(err = ?err, "Failed to fetch wallet state");
                Self::send_err(
                    tx,
                    WalletServiceError::FailedToFetchWalletStateForBlock(tip),
                );
                return;
            }
        };

        let aged_utxos = ledger_state.epoch_state().utxos.utxos();
        let eligible_utxos: Vec<Utxo> = wallet_state
            .utxos
            .iter()
            .filter(|(note_id, _)| aged_utxos.contains_key(note_id))
            .map(|(_, utxo)| *utxo)
            .collect();

        if tx.send(Ok(eligible_utxos)).is_err() {
            error!("Failed to respond to GetLeaderAgedNotes");
        }
    }

    async fn backfill_if_not_in_sync(
        tip: HeaderId,
        wallet: &mut Wallet,
        storage: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    ) -> Result<(), WalletServiceError> {
        if wallet.has_processed_block(tip) {
            // We are already in sync with `tip`.
            return Ok(());
        }

        // The caller knows a more recent tip than the wallet.
        // To resolve this, we do a JIT backfill to try to sync the wallet with
        // cryptarchia. If we still have not caught up after the backfill, we return an
        // error to the caller
        let headers = Self::fetch_missing_headers(tip, cryptarchia).await?;
        Self::backfill_missing_blocks(&headers, wallet, storage).await?;

        if wallet.has_processed_block(tip) {
            Ok(())
        } else {
            error!("Failed to backfill wallet to {tip}");
            Err(WalletServiceError::FailedToFetchWalletStateForBlock(tip))
        }
    }

    fn handle_lib_update(lib_update: &LibUpdate, wallet: &mut Wallet) {
        debug!(
            new_lib = ?lib_update.new_lib,
            stale_blocks_count = lib_update.pruned_blocks.stale_blocks.len(),
            immutable_blocks_count = lib_update.pruned_blocks.immutable_blocks.len(),
            "Received LIB update"
        );

        wallet.prune_states(lib_update.pruned_blocks.all());
    }

    async fn fetch_missing_headers(
        missing_block: HeaderId,
        cryptarchia_api: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    ) -> Result<Vec<HeaderId>, WalletServiceError> {
        cryptarchia_api
            .get_headers_to_lib(missing_block)
            .await
            .map_err(WalletServiceError::CryptarchiaApi)
    }

    async fn backfill_missing_blocks(
        headers: &[HeaderId],
        wallet: &mut Wallet,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> Result<(), WalletServiceError> {
        for header_id in headers.iter().rev().copied() {
            if wallet.has_processed_block(header_id) {
                info!("skipping already processed block");
                continue;
            }

            let Some(block) = storage_adapter.get_block(&header_id).await else {
                error!(block_id = ?header_id, "Block not found in storage during wallet sync");
                return Err(WalletServiceError::BlockNotFoundInStorage(header_id));
            };

            if let Err(e) = wallet.apply_block(&block.into()) {
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
        tx: oneshot::Sender<Result<T, WalletServiceError>>,
        err: WalletServiceError,
    ) {
        if let Err(msg) = tx.send(Err(err)) {
            error!(msg = ?msg, "Wallet failed to send error response");
        }
    }
}
