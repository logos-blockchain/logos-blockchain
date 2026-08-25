pub mod api;
pub mod mempool;
mod metrics;
pub mod state;
pub mod wallet;

use std::fmt::{Debug, Display};

use async_trait::async_trait;
use lb_chain_service::{
    ChainServiceInfo,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    header::HeaderId,
    mantle::{
        NoteId, SignedMantleTx,
        traits::Hashable as _,
        transactions::{MantleTxBuilder, states::Preverified},
    },
    sdp::{
        ActiveMessage, ActivityMetadata, DeclarationId, DeclarationMessage, ProviderId,
        WithdrawMessage,
    },
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_services_utils::overwatch::{RecoveryData, RecoveryOperator, StorageRecoverySettings};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::oneshot;

pub use crate::api::SdpServiceApi;
use crate::{
    mempool::SdpMempoolAdapter,
    state::{SdpState, SdpStateStorage},
    wallet::{SdpWalletAdapter, SdpWalletConfig},
};

fn activity_proof_epoch(metadata: &ActivityMetadata) -> u32 {
    match metadata {
        ActivityMetadata::Blend(proof) => u32::from(proof.epoch),
    }
}
#[derive(Debug, Error)]
pub enum SdpError {
    #[error("Declaration {0:?} not found in ledger")]
    DeclarationNotFound(DeclarationId),

    #[error("Ledger state not found for block {0:?}")]
    LedgerStateNotFound(HeaderId),

    #[error("Chain API error: {0}")]
    ChainApi(#[from] DynError),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SdpSettings {
    /// Declaration ID for this node (set after posting declaration).
    /// On startup, the full declaration info (`zk_id`, `locked_note_id`, nonce)
    /// will be fetched from the ledger.
    pub declaration_id: Option<DeclarationId>,
    pub wallet_config: SdpWalletConfig,
    #[serde(skip)]
    pub recovery_data: RecoveryData,
}

impl StorageRecoverySettings for SdpSettings {
    const RECOVERY_KEY_SUFFIX: &'static [u8] = b"sdp";

    fn recovery_data(&self) -> &RecoveryData {
        &self.recovery_data
    }
}

/// Runtime declaration info fetched from ledger on startup.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeDeclaration {
    pub id: DeclarationId,
    pub zk_id: ZkPublicKey,
    pub locked_note_id: NoteId,
    pub nonce: u64,
}

#[derive(Clone, Debug)]
struct RuntimeDeclarationContext {
    declaration: RuntimeDeclaration,
    chain_epoch: u32,
    chain_slot: u64,
    provider_id: ProviderId,
}

pub enum SdpMessage {
    PostDeclaration {
        declaration: Box<DeclarationMessage>,
        reply_channel: oneshot::Sender<Result<DeclarationId, DynError>>,
    },
    PostActivity {
        metadata: ActivityMetadata, // DA/Blend specific metadata
    },
    PostWithdrawal {
        declaration_id: DeclarationId,
    },
    SetCurrentDeclarationId {
        declaration_id: Option<DeclarationId>,
        reply_channel: oneshot::Sender<Result<(), SdpError>>,
    },
}

pub struct SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>
where
    StateStorage: SdpStateStorage<RuntimeServiceId>,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    declaration_id: Option<DeclarationId>,
    wallet_config: SdpWalletConfig,
    _phantom: std::marker::PhantomData<(ChainService, StateStorage)>,
}

impl<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId> ServiceData
    for SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>
where
    StateStorage: SdpStateStorage<RuntimeServiceId>,
{
    type Settings = SdpSettings;
    type State = SdpState;
    type StateOperator = RecoveryOperator<StateStorage>;
    type Message = SdpMessage;
}

#[async_trait]
impl<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>
    ServiceCore<RuntimeServiceId>
    for SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>
where
    MempoolAdapter: SdpMempoolAdapter<Tx = SignedMantleTx<Preverified>> + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData<Tx = SignedMantleTx<Preverified>> + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId> + Send + Sync,
    RuntimeServiceId: Debug
        + AsServiceId<Self>
        + AsServiceId<MempoolAdapter::MempoolService>
        + AsServiceId<WalletAdapter::WalletService>
        + AsServiceId<ChainService>
        + Clone
        + Display
        + Send
        + Sync
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let declaration_id = initial_state
            .updated
            .and(initial_state.declaration_id)
            .or(settings.declaration_id);

        Ok(Self {
            declaration_id,
            service_resources_handle,
            wallet_config: settings.wallet_config,
            _phantom: std::marker::PhantomData,
        })
    }

    async fn run(mut self) -> Result<(), DynError> {
        let mempool_relay = self
            .service_resources_handle
            .overwatch_handle
            .relay::<MempoolAdapter::MempoolService>()
            .await?;
        let mempool_adapter = MempoolAdapter::new(mempool_relay);

        let wallet_relay = self
            .service_resources_handle
            .overwatch_handle
            .relay::<WalletAdapter::WalletService>()
            .await?;
        let wallet_adapter = WalletAdapter::new(wallet_relay);

        let chain_relay = self
            .service_resources_handle
            .overwatch_handle
            .relay::<ChainService>()
            .await?;
        let chain_api: CryptarchiaServiceApi<ChainService, RuntimeServiceId> =
            CryptarchiaServiceApi::new(chain_relay);

        self.validate_initial_declaration_status(&chain_api).await?;

        self.service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        while let Some(msg) = self.service_resources_handle.inbound_relay.recv().await {
            match msg {
                SdpMessage::PostActivity { metadata, .. } => {
                    metrics::activity_posts_total();

                    self.handle_post_activity(
                        metadata,
                        &wallet_adapter,
                        &mempool_adapter,
                        &chain_api,
                    )
                    .await;
                }
                SdpMessage::PostDeclaration {
                    declaration,
                    reply_channel,
                } => {
                    metrics::declarations_total();

                    self.handle_post_declaration(
                        declaration,
                        &wallet_adapter,
                        &mempool_adapter,
                        reply_channel,
                    )
                    .await;
                }
                SdpMessage::PostWithdrawal { declaration_id } => {
                    metrics::withdrawals_total();

                    self.handle_post_withdrawal(
                        declaration_id,
                        &wallet_adapter,
                        &mempool_adapter,
                        &chain_api,
                    )
                    .await;
                }
                SdpMessage::SetCurrentDeclarationId {
                    declaration_id,
                    reply_channel,
                } => {
                    self.handle_set_current_declaration_id(
                        declaration_id,
                        reply_channel,
                        &chain_api,
                    )
                    .await;
                }
            }
        }

        Ok(())
    }
}

impl<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>
    SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>
where
    MempoolAdapter: SdpMempoolAdapter<Tx = SignedMantleTx<Preverified>> + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData<Tx = SignedMantleTx<Preverified>> + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId> + Send + Sync,
    RuntimeServiceId: Debug
        + AsServiceId<Self>
        + AsServiceId<MempoolAdapter::MempoolService>
        + AsServiceId<ChainService>
        + Clone
        + Display
        + Send
        + Sync
        + 'static,
{
    /// Attempt to restore declaration state from the ledger on startup.
    ///
    /// If a `declaration_id` is configured, fetches the full declaration info
    /// (including current nonce) from the ledger. This ensures the service
    /// continues with the correct nonce after a restart.
    async fn try_fetch_runtime_declaration(
        &self,
        declaration_id: DeclarationId,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) -> Result<RuntimeDeclarationContext, SdpError> {
        self.fetch_declaration_from_ledger(chain_api, declaration_id)
            .await?
            .map_or_else(
                || {
                    tracing::warn!(?declaration_id, "Declaration not found in ledger");
                    Err(SdpError::DeclarationNotFound(declaration_id))
                },
                |declaration| {
                    tracing::info!(
                        ?declaration.declaration.id,
                        declaration.declaration.nonce,
                        "Loaded declaration from ledger"
                    );
                    Ok(declaration)
                },
            )
    }

    /// Fetch declaration info from the ledger via chain service.
    async fn fetch_declaration_from_ledger(
        &self,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
        declaration_id: DeclarationId,
    ) -> Result<Option<RuntimeDeclarationContext>, DynError> {
        // Get current chain info to find the tip
        let ChainServiceInfo {
            cryptarchia_info, ..
        } = chain_api.info().await?;
        let tip = cryptarchia_info.tip;
        tracing::debug!(
            "Fetching declaration state for {declaration_id:?} from ledger tip {tip:?}"
        );

        // Get ledger state at tip
        let Some(ledger_state) = chain_api.get_ledger_state(tip).await? else {
            return Err(format!("Ledger state not found for tip {tip:?}").into());
        };

        // Look up the declaration in the SDP ledger
        let sdp_ledger = ledger_state.mantle_ledger().sdp_ledger();
        let Some(declaration) = sdp_ledger.get_declaration(&declaration_id) else {
            return Ok(None);
        };

        Ok(Some(RuntimeDeclarationContext {
            declaration: RuntimeDeclaration {
                id: declaration_id,
                zk_id: declaration.zk_id,
                locked_note_id: declaration.locked_note_id,
                nonce: declaration.nonce,
            },
            chain_epoch: u32::from(ledger_state.epoch_state().epoch),
            chain_slot: u64::from(ledger_state.slot()),
            provider_id: declaration.provider_id,
        }))
    }

    async fn validate_initial_declaration_status(
        &self,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) -> Result<(), DynError> {
        let Some(id) = self.declaration_id else {
            return Ok(());
        };

        match self.try_fetch_runtime_declaration(id, chain_api).await {
            Ok(_) => Ok(()),
            Err(e) => match e {
                SdpError::ChainApi(err) => {
                    tracing::error!("Chain API error during declaration resolution: {err}");
                    Err(err)
                }
                SdpError::DeclarationNotFound(id) => {
                    tracing::warn!(
                        declaration_id = ?id,
                        "Declaration not found in ledger"
                    );
                    Ok(())
                }
                SdpError::LedgerStateNotFound(tip) => {
                    tracing::error!("Could not find ledger state for tip {tip:?}");
                    Err(format!("Missing ledger state at {tip:?}").into())
                }
            },
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn handle_post_declaration(
        &mut self,
        declaration: Box<DeclarationMessage>,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        reply_channel: oneshot::Sender<Result<DeclarationId, DynError>>,
    ) {
        let tx_builder = MantleTxBuilder::new();
        let declaration_id = declaration.id();

        tracing::debug!(
            diagnostic = "blend_tsi_outage",
            event = "sdp_declaration_submitted",
            provider_id = ?declaration.provider_id,
            declaration_id = ?declaration_id,
            zk_id = ?declaration.zk_id,
            "Created SDP declaration transaction"
        );

        let signed_tx = match wallet_adapter
            .declare_tx(tx_builder, *declaration, &self.wallet_config)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                tracing::error!("Failed to create declaration transaction: {:?}", e);
                metrics::declaration_tx_failures_total();
                return;
            }
        };

        if let Err(e) = mempool_adapter.post_tx(signed_tx).await {
            tracing::error!("Failed to post declaration to mempool: {:?}", e);
            metrics::declaration_mempool_failures_total();
            return;
        }

        if let Err(e) = reply_channel.send(Ok(declaration_id)) {
            tracing::error!("Failed to send post declaration response: {:?}", e);
        } else {
            metrics::declaration_success_total();
        }

        self.declaration_id = Some(declaration_id);
        self.service_resources_handle
            .state_updater
            .update(Some(SdpState::from(self.declaration_id)));
    }

    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn handle_post_activity(
        &self,
        metadata: ActivityMetadata,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        let Some(declaration_id) = self.declaration_id else {
            tracing::error!("No declaration_id set. Cannot post activity without declaration.");
            return;
        };

        let Ok(RuntimeDeclarationContext {
            declaration,
            chain_epoch,
            chain_slot,
            provider_id,
        }) = self
            .try_fetch_runtime_declaration(declaration_id, chain_api)
            .await
        else {
            tracing::error!("Can't find declaration. Cannot post activity without declaration.");
            return;
        };

        let Some(nonce) = declaration.nonce.checked_add(1) else {
            tracing::error!("Can't bump nonce");
            return;
        };

        let proof_epoch = activity_proof_epoch(&metadata);

        tracing::info!(
            diagnostic = "blend_tsi_outage",
            event = "sdp_activity_submission_requested",
            proof_epoch,
            chain_epoch,
            chain_slot,
            provider_id = ?provider_id,
            zk_id = ?declaration.zk_id,
            declaration_id = ?declaration.id,
            "Requested SDP activity transaction submission"
        );

        let active_message = ActiveMessage {
            declaration_id: declaration.id,
            nonce,
            metadata,
        };

        let tx_builder = MantleTxBuilder::new();

        let signed_tx = match wallet_adapter
            .active_tx(tx_builder, active_message, &self.wallet_config)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!(
                    diagnostic = "blend_tsi_outage",
                    event = "sdp_activity_tx_failed",
                    proof_epoch,
                    chain_epoch,
                    chain_slot,
                    provider_id = ?provider_id,
                    zk_id = ?declaration.zk_id,
                    declaration_id = ?declaration.id,
                    stage = "create",
                    error = %e,
                    "Failed to create SDP activity transaction"
                );
                metrics::activity_tx_failures_total();
                return;
            }
        };

        let tx_id = signed_tx.hash();
        tracing::debug!(
            diagnostic = "blend_tsi_outage",
            event = "sdp_activity_tx_created",
            proof_epoch,
            chain_epoch,
            chain_slot,
            provider_id = ?provider_id,
            zk_id = ?declaration.zk_id,
            declaration_id = ?declaration.id,
            tx_id = ?tx_id,
            "Created SDP activity transaction"
        );

        if let Err(e) = mempool_adapter.post_tx(signed_tx).await {
            tracing::warn!(
                diagnostic = "blend_tsi_outage",
                event = "sdp_activity_tx_failed",
                proof_epoch,
                chain_epoch,
                chain_slot,
                provider_id = ?provider_id,
                zk_id = ?declaration.zk_id,
                declaration_id = ?declaration.id,
                stage = "submit",
                tx_id = ?tx_id,
                error = %e,
                "Failed to submit SDP activity transaction"
            );
            metrics::activity_mempool_failures_total();
        } else {
            tracing::info!(
                diagnostic = "blend_tsi_outage",
                event = "sdp_activity_tx_submitted",
                proof_epoch,
                chain_epoch,
                chain_slot,
                provider_id = ?provider_id,
                zk_id = ?declaration.zk_id,
                declaration_id = ?declaration.id,
                tx_id = ?tx_id,
                "Submitted SDP activity transaction"
            );
            metrics::activity_success_total();
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn handle_post_withdrawal(
        &mut self,
        declaration_id: DeclarationId,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        let Ok(RuntimeDeclarationContext { declaration, .. }) = self
            .try_fetch_runtime_declaration(declaration_id, chain_api)
            .await
        else {
            tracing::error!("Can't find declaration. Cannot post activity without declaration.");
            metrics::withdrawal_validation_failures_total();
            return;
        };

        let Some(nonce) = declaration.nonce.checked_add(1) else {
            tracing::error!("Can't bump nonce");
            metrics::withdrawal_validation_failures_total();
            return;
        };

        let withdraw_message = WithdrawMessage {
            declaration_id,
            locked_note_id: declaration.locked_note_id,
            nonce,
        };

        let tx_builder = MantleTxBuilder::new();

        let signed_tx = match wallet_adapter
            .withdraw_tx(tx_builder, withdraw_message, &self.wallet_config)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                tracing::error!("Failed to create withdrawal transaction: {:?}", e);
                metrics::withdrawal_tx_failures_total();
                return;
            }
        };

        if let Err(e) = mempool_adapter.post_tx(signed_tx).await {
            tracing::error!("Failed to post withdrawal to mempool: {:?}", e);
            metrics::withdrawal_mempool_failures_total();
            return;
        }

        metrics::withdrawal_success_total();

        self.declaration_id = None;
        self.service_resources_handle
            .state_updater
            .update(Some(SdpState::from(self.declaration_id)));
    }

    async fn handle_set_current_declaration_id(
        &mut self,
        declaration_id: Option<DeclarationId>,
        reply_channel: oneshot::Sender<Result<(), SdpError>>,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        let result = self
            .validate_declaration_id(declaration_id, chain_api)
            .await;

        if let Err(e) = reply_channel.send(result) {
            tracing::error!("Failed to send response for set declaration: {e:?}");
        }
    }

    async fn validate_declaration_id(
        &mut self,
        declaration_id: Option<DeclarationId>,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) -> Result<(), SdpError> {
        let validated_id = match declaration_id {
            Some(id) => self
                .try_fetch_runtime_declaration(id, chain_api)
                .await
                .map(|_| Some(id))?,
            None => None,
        };

        self.declaration_id = validated_id;
        self.service_resources_handle
            .state_updater
            .update(Some(SdpState::from(self.declaration_id)));

        Ok(())
    }
}
