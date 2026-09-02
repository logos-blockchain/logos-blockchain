pub mod api;
mod intent;
pub mod mempool;
mod metrics;
pub mod state;
pub mod wallet;

use std::fmt::{Debug, Display};

use async_trait::async_trait;
use lb_chain_service::{
    ChainServiceInfo, Epoch, ProcessedBlockEvent,
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
use lb_ledger::{Intent, IntentStatus, LedgerState};
use lb_log_targets::sdp;
use lb_services_utils::overwatch::{RecoveryData, RecoveryOperator, StorageRecoverySettings};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::{debug, error, trace, warn};

pub use crate::{api::SdpServiceApi, intent::Config as ActiveMessageTrackerConfig};
use crate::{
    intent::IntentTracker,
    mempool::SdpMempoolAdapter,
    state::{SdpState, SdpStateStorage},
    wallet::{SdpWalletAdapter, SdpWalletConfig},
};

const LOG_TARGET: &str = sdp::ROOT;

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
    /// On startup, the full declaration info (`zk_id`, `service_note_id`,
    /// nonce) will be fetched from the ledger.
    pub declaration_id: Option<DeclarationId>,
    pub wallet_config: SdpWalletConfig,
    pub active_message_tracker: intent::Config,
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
    pub service_note_id: NoteId,
    pub nonce: u64,
    pub tip: HeaderId,
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
    ChainService: CryptarchiaServiceData,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    declaration_id: Option<DeclarationId>,
    wallet_config: SdpWalletConfig,
    active_message_tracker_config: intent::Config,
    active_message_tracker:
        Option<IntentTracker<Activity, CryptarchiaServiceApi<ChainService, RuntimeServiceId>>>,
    /// Activity message restored from the previous run, to be tracked again.
    restored_activity: Option<Activity>,
    _phantom: std::marker::PhantomData<(ChainService, StateStorage)>,
}

impl<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId> ServiceData
    for SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>
where
    ChainService: CryptarchiaServiceData,
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
            active_message_tracker_config: settings.active_message_tracker,
            active_message_tracker: None,
            restored_activity: initial_state.pending_activity,
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
        self.restore_active_message_tracker(&chain_api).await;

        let mut new_blocks = chain_api.subscribe_new_blocks().await?;

        self.service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        loop {
            tokio::select! {
                Some(msg) = self.service_resources_handle.inbound_relay.recv() => {
                    self.handle_message(msg, &wallet_adapter, &mempool_adapter, &chain_api).await;
                }
                Ok(event) = new_blocks.recv() => {
                    self.handle_new_block(event, &wallet_adapter, &mempool_adapter, &chain_api).await;
                }
            }
        }
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
    async fn handle_message(
        &mut self,
        msg: SdpMessage,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        match msg {
            SdpMessage::PostActivity { metadata, .. } => {
                metrics::activity_posts_total();

                self.handle_post_activity(metadata, wallet_adapter, mempool_adapter, chain_api)
                    .await;
            }
            SdpMessage::PostDeclaration {
                declaration,
                reply_channel,
            } => {
                metrics::declarations_total();

                self.handle_post_declaration(
                    declaration,
                    wallet_adapter,
                    mempool_adapter,
                    reply_channel,
                )
                .await;
            }
            SdpMessage::PostWithdrawal { declaration_id } => {
                metrics::withdrawals_total();

                self.handle_post_withdrawal(
                    declaration_id,
                    wallet_adapter,
                    mempool_adapter,
                    chain_api,
                )
                .await;
            }
            SdpMessage::SetCurrentDeclarationId {
                declaration_id,
                reply_channel,
            } => {
                self.handle_set_current_declaration_id(declaration_id, reply_channel, chain_api)
                    .await;
            }
        }
    }

    async fn handle_new_block(
        &mut self,
        event: ProcessedBlockEvent,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        let Some(tracker) = self.active_message_tracker.as_mut() else {
            trace!(target: LOG_TARGET, "no active message tracker exists");
            return;
        };

        match tracker.handle_tip(event.tip, event.lib).await {
            Ok(outcome) => {
                self.handle_active_message_tracker_outcome(
                    outcome,
                    wallet_adapter,
                    mempool_adapter,
                    chain_api,
                )
                .await;
            }
            Err(err) => {
                error!(target: LOG_TARGET, %err, "active message tracker failed to handle tip");
            }
        }
    }

    async fn handle_active_message_tracker_outcome(
        &mut self,
        outcome: intent::Outcome<Activity>,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        match outcome {
            intent::Outcome::StatusChecked {
                intent: activity,
                status,
            } => {
                self.handle_active_message_status(
                    activity,
                    status,
                    wallet_adapter,
                    mempool_adapter,
                    chain_api,
                )
                .await;
            }
            intent::Outcome::WaitingforMoreTipChanges => {
                trace!(target: LOG_TARGET, "active message tracker waiting for more tip changes before status check");
            }
            intent::Outcome::Finalized => {
                debug!(target: LOG_TARGET, "active message intent has been finalized in the LIB: dropping the tracker");
                self.active_message_tracker = None;
                self.update_state();
            }
        }
    }

    async fn handle_active_message_status(
        &self,
        activity: Activity,
        status: IntentStatus,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        match status {
            IntentStatus::NotApplied => {
                trace!(target: LOG_TARGET, "active message status: not applied in the tip ledger: resubmitting it");
                self.submit_activity(activity, wallet_adapter, mempool_adapter, chain_api)
                    .await;
            }
            IntentStatus::Applied => {
                trace!(target: LOG_TARGET, "active message status: applied in the tip ledger: keep tracking it");
            }
        }
    }

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
                    tracing::warn!(target: LOG_TARGET, ?declaration_id, "Declaration not found in ledger");
                    Err(SdpError::DeclarationNotFound(declaration_id))
                },
                |declaration| {
                    tracing::info!(
                        target: LOG_TARGET,
                        {
                            declaration.declaration.id = ?declaration.declaration.id,
                            declaration.declaration.nonce = declaration.declaration.nonce,
                        },
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
            target: LOG_TARGET,
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
                service_note_id: declaration.service_note_id,
                nonce: declaration.nonce,
                tip,
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
                    tracing::error!(target: LOG_TARGET, "Chain API error during declaration resolution: {err}");
                    Err(err)
                }
                SdpError::DeclarationNotFound(id) => {
                    tracing::warn!(
                        target: LOG_TARGET,
                        declaration_id = ?id,
                        "Declaration not found in ledger"
                    );
                    Ok(())
                }
                SdpError::LedgerStateNotFound(tip) => {
                    tracing::error!(target: LOG_TARGET, "Could not find ledger state for tip {tip:?}");
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
        let provider_id = declaration.provider_id;
        let zk_id = declaration.zk_id;

        tracing::debug!(
            target: LOG_TARGET,
            diagnostic = "blend_tsi_outage",
            event = "sdp_declaration_submission_requested",
            provider_id = ?provider_id,
            declaration_id = ?declaration_id,
            zk_id = ?zk_id,
            "Requested SDP declaration transaction submission"
        );

        let signed_tx = match wallet_adapter
            .declare_tx(tx_builder, *declaration, &self.wallet_config)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                tracing::error!(target: LOG_TARGET, "Failed to create declaration transaction: {:?}", e);
                metrics::declaration_tx_failures_total();
                return;
            }
        };

        let tx_id = signed_tx.hash();
        tracing::debug!(
            target: LOG_TARGET,
            diagnostic = "blend_tsi_outage",
            event = "sdp_declaration_tx_created",
            provider_id = ?provider_id,
            declaration_id = ?declaration_id,
            zk_id = ?zk_id,
            tx_id = ?tx_id,
            "Created SDP declaration transaction"
        );

        if let Err(e) = mempool_adapter.post_tx(signed_tx).await {
            tracing::error!(target: LOG_TARGET, "Failed to post declaration to mempool: {:?}", e);
            metrics::declaration_mempool_failures_total();
            return;
        }

        tracing::info!(
            target: LOG_TARGET,
            diagnostic = "blend_tsi_outage",
            event = "sdp_declaration_submitted",
            provider_id = ?provider_id,
            declaration_id = ?declaration_id,
            zk_id = ?zk_id,
            tx_id = ?tx_id,
            "Submitted SDP declaration transaction"
        );

        if let Err(e) = reply_channel.send(Ok(declaration_id)) {
            tracing::error!(target: LOG_TARGET, "Failed to send post declaration response: {:?}", e);
        } else {
            metrics::declaration_success_total();
        }

        self.declaration_id = Some(declaration_id);
        self.update_state();
    }

    async fn handle_post_activity(
        &mut self,
        metadata: ActivityMetadata,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        let Some(declaration_id) = self.declaration_id else {
            tracing::error!(target: LOG_TARGET, "No declaration_id set. Cannot post activity without declaration.");
            return;
        };

        let activity = Activity {
            declaration_id,
            metadata,
        };

        let tip = self
            .submit_activity(activity.clone(), wallet_adapter, mempool_adapter, chain_api)
            .await
            .map_or_default(Some);

        if self
            .active_message_tracker
            .replace(IntentTracker::new(
                activity,
                self.active_message_tracker_config.clone(),
                tip,
                chain_api.clone(),
            ))
            .is_some()
        {
            debug!(target: LOG_TARGET, "active message tracker replaced");
        }
        self.update_state();
    }

    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn submit_activity(
        &self,
        activity: Activity,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) -> Option<HeaderId> {
        trace!(
            target: LOG_TARGET,
            epoch = ?activity.metadata.submission_epoch(),
            "submitting activity message"
        );

        let Ok(RuntimeDeclarationContext {
            declaration,
            chain_epoch,
            chain_slot,
            provider_id,
        }) = self
            .try_fetch_runtime_declaration(activity.declaration_id, chain_api)
            .await
        else {
            tracing::error!(target: LOG_TARGET, "Can't find declaration. Cannot post activity without declaration.");
            return None;
        };

        let Some(nonce) = declaration.nonce.checked_add(1) else {
            tracing::error!(target: LOG_TARGET, "Can't bump nonce");
            return None;
        };

        let proof_epoch = u32::from(activity.metadata.origin_epoch());

        tracing::debug!(
            target: LOG_TARGET,
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
            metadata: activity.metadata,
        };

        let tx_builder = MantleTxBuilder::new();

        let signed_tx = match wallet_adapter
            .active_tx(tx_builder, active_message, &self.wallet_config)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                tracing::error!(
                    target: LOG_TARGET,
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
                return None;
            }
        };

        let tx_id = signed_tx.hash();
        tracing::debug!(
            target: LOG_TARGET,
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
            tracing::error!(
                target: LOG_TARGET,
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
            return None;
        }

        tracing::info!(
            target: LOG_TARGET,
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
        Some(declaration.tip)
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
            tracing::error!(target: LOG_TARGET, "Can't find declaration. Cannot post activity without declaration.");
            metrics::withdrawal_validation_failures_total();
            return;
        };

        let Some(nonce) = declaration.nonce.checked_add(1) else {
            tracing::error!(target: LOG_TARGET, "Can't bump nonce");
            metrics::withdrawal_validation_failures_total();
            return;
        };

        let withdraw_message = WithdrawMessage {
            declaration_id,
            service_note_id: declaration.service_note_id,
            nonce,
        };

        let tx_builder = MantleTxBuilder::new();

        let signed_tx = match wallet_adapter
            .withdraw_tx(tx_builder, withdraw_message, &self.wallet_config)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                tracing::error!(target: LOG_TARGET, "Failed to create withdrawal transaction: {:?}", e);
                metrics::withdrawal_tx_failures_total();
                return;
            }
        };

        if let Err(e) = mempool_adapter.post_tx(signed_tx).await {
            tracing::error!(target: LOG_TARGET, "Failed to post withdrawal to mempool: {:?}", e);
            metrics::withdrawal_mempool_failures_total();
            return;
        }

        metrics::withdrawal_success_total();

        self.declaration_id = None;
        self.active_message_tracker = None;
        self.update_state();
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
            tracing::error!(target: LOG_TARGET, "Failed to send response for set declaration: {e:?}");
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
        self.update_state();

        Ok(())
    }

    /// Tracks the active message restored from the previous run again,
    /// unless it belongs to another declaration or its submission epoch has
    /// already passed.
    async fn restore_active_message_tracker(
        &mut self,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        let Some(activity) = self.restored_activity.take() else {
            return;
        };

        match self
            .validate_restored_active_message(&activity, chain_api)
            .await
        {
            Ok(()) => {
                debug!(target: LOG_TARGET, "creating tracker for the restored active message");
                self.active_message_tracker = Some(IntentTracker::new(
                    activity,
                    self.active_message_tracker_config.clone(),
                    None,
                    chain_api.clone(),
                ));
            }
            Err(reason) => {
                warn!(target: LOG_TARGET, %reason, "dropping the stale restored active message");
                self.update_state();
            }
        }
    }

    async fn validate_restored_active_message(
        &self,
        activity: &Activity,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) -> Result<(), DynError> {
        let tip_epoch = self.fetch_tip_epoch(chain_api).await?;
        Ok(activity.validate(self.declaration_id, tip_epoch)?)
    }

    async fn fetch_tip_epoch(
        &self,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) -> Result<Epoch, DynError> {
        let tip = chain_api.info().await?.cryptarchia_info.tip;
        let ledger_state = chain_api
            .get_ledger_state(tip)
            .await?
            .ok_or_else(|| format!("ledger state not found for tip {tip:?}"))?;
        Ok(ledger_state.epoch_state().epoch)
    }

    /// Updates/persists the service state.
    fn update_state(&self) {
        let pending_activity = self
            .active_message_tracker
            .as_ref()
            .map(|tracker| tracker.intent().clone());
        self.service_resources_handle
            .state_updater
            .update(Some(SdpState::new(self.declaration_id, pending_activity)));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    declaration_id: DeclarationId,
    metadata: ActivityMetadata,
}

impl Intent for Activity {
    type Error = IntentStatusCheckFailed;

    /// The intent of an active message is to refresh the `Declaration::active`
    /// field.
    fn status(&self, ledger: &LedgerState) -> Result<IntentStatus, Self::Error> {
        let declaration = ledger
            .mantle_ledger()
            .sdp_ledger()
            .get_declaration(&self.declaration_id)
            .ok_or_else(|| IntentStatusCheckFailed("declaration not exist".to_owned()))?;

        // Check if the `active` field has been refreshed.
        if declaration.active >= self.metadata.submission_epoch() {
            Ok(IntentStatus::Applied)
        } else {
            Ok(IntentStatus::NotApplied)
        }
    }
}

impl Activity {
    /// Validates [`Activity`] against `declaration_id` and `tip_epoch`.
    ///
    /// If the activity has a different declaration ID or has the submission
    /// epoch older than the `tip_epoch`, this function returns an error.
    ///
    /// This can be used to check whether an active message restored from
    /// the service state is still valid.
    fn validate(
        &self,
        declaration_id: Option<DeclarationId>,
        tip_epoch: Epoch,
    ) -> Result<(), InvalidActiveMessageError> {
        if Some(self.declaration_id) != declaration_id {
            return Err(InvalidActiveMessageError::AnotherDeclaration);
        }

        let submission_epoch = self.metadata.submission_epoch();
        if submission_epoch < tip_epoch {
            return Err(InvalidActiveMessageError::Stale {
                submission_epoch,
                tip_epoch,
            });
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum InvalidActiveMessageError {
    #[error("active message belongs to another declaration")]
    AnotherDeclaration,
    #[error(
        "active message is stale: submission_epoch({submission_epoch:?}) < tip_epoch({tip_epoch:?})"
    )]
    Stale {
        submission_epoch: Epoch,
        tip_epoch: Epoch,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct IntentStatusCheckFailed(String);

#[async_trait]
impl<ChainService, RuntimeServiceId> intent::LedgerStateProvider
    for CryptarchiaServiceApi<ChainService, RuntimeServiceId>
where
    ChainService: CryptarchiaServiceData<Tx: Send + Sync> + Send + Sync,
    RuntimeServiceId: AsServiceId<ChainService> + Send + Sync,
{
    type Error = lb_chain_service::api::ApiError;

    async fn get(&self, block: HeaderId) -> Result<Option<LedgerState>, Self::Error> {
        Ok(self.get_ledger_state(block).await?)
    }
}

#[cfg(test)]
mod tests {
    use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
    use lb_core::sdp::blend::ActivityProof;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;

    const DECLARATION_ID: DeclarationId = DeclarationId([1; 32]);

    #[test]
    fn restored_active_message_of_another_declaration_is_invalid() {
        let activity = activity(1);
        assert!(matches!(
            activity.validate(Some(DeclarationId([2; 32])), 2.into()),
            Err(InvalidActiveMessageError::AnotherDeclaration)
        ));
        assert!(matches!(
            activity.validate(None, 2.into()),
            Err(InvalidActiveMessageError::AnotherDeclaration)
        ));
    }

    #[test]
    fn stale_restored_active_message_is_invalid() {
        let activity = activity(1);
        assert_eq!(activity.metadata.submission_epoch(), Epoch::new(2));
        assert!(matches!(
            activity.validate(Some(DECLARATION_ID), 3.into()),
            Err(InvalidActiveMessageError::Stale { .. })
        ));
    }

    #[test]
    fn restored_active_message_not_older_than_tip_epoch_is_valid() {
        let activity = activity(1);
        assert_eq!(activity.metadata.submission_epoch(), Epoch::new(2));
        activity.validate(Some(DECLARATION_ID), 2.into()).unwrap();
        // the tip epoch lags behind the submission epoch
        activity.validate(Some(DECLARATION_ID), 1.into()).unwrap();
    }

    fn activity(proof_epoch: u32) -> Activity {
        Activity {
            declaration_id: DECLARATION_ID,
            metadata: ActivityMetadata::Blend(Box::new(ActivityProof {
                epoch: proof_epoch.into(),
                signing_key: Ed25519Key::from_bytes(&[0; _]).public_key(),
                proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]).into(),
                proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]).into(),
            })),
        }
    }
}
