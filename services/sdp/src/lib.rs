pub mod mempool;
mod metrics;
pub mod wallet;

use std::{
    collections::BTreeSet,
    fmt::{Debug, Display},
    pin::Pin,
};

use async_trait::async_trait;
use futures::Stream;
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    block::BlockNumber,
    mantle::{NoteId, SignedMantleTx, tx_builder::MantleTxBuilder},
    sdp::{
        ActiveMessage, ActivityMetadata, DeclarationId, DeclarationMessage, Locator, ProviderId,
        ServiceType, WithdrawMessage,
    },
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::{
    mempool::SdpMempoolAdapter,
    wallet::{SdpWalletAdapter, SdpWalletConfig},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclarationState {
    Active,
    Inactive,
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockEventUpdate {
    pub service_type: ServiceType,
    pub provider_id: ProviderId,
    pub state: DeclarationState,
    pub locators: BTreeSet<Locator>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockEvent {
    pub block_number: BlockNumber,
    pub updates: Vec<BlockEventUpdate>,
}

pub type BlockUpdateStream = Pin<Box<dyn Stream<Item = BlockEvent> + Send + Sync + Unpin>>;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SdpSettings {
    /// Declaration ID for this node (set after posting declaration).
    /// On startup, the full declaration info (`zk_id`, `locked_note_id`, nonce)
    /// will be fetched from the ledger.
    pub declaration_id: Option<DeclarationId>,
    pub wallet_config: SdpWalletConfig,
}

/// Runtime declaration info fetched from ledger on startup.
#[derive(Clone, Debug)]
pub struct RuntimeDeclaration {
    pub id: DeclarationId,
    pub zk_id: ZkPublicKey,
    pub locked_note_id: NoteId,
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
}

pub struct SdpService<MempoolAdapter, WalletAdapter, ChainService, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    /// Declaration ID from settings - will be resolved to full declaration on
    /// `run()`.
    declaration_id: Option<DeclarationId>,
    /// Runtime declaration info fetched from ledger (populated in `run()`).
    current_declaration: Option<RuntimeDeclaration>,
    nonce: u64,
    wallet_config: SdpWalletConfig,
    _chain_service: std::marker::PhantomData<ChainService>,
}

impl<MempoolAdapter, WalletAdapter, ChainService, RuntimeServiceId> ServiceData
    for SdpService<MempoolAdapter, WalletAdapter, ChainService, RuntimeServiceId>
{
    type Settings = SdpSettings;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = SdpMessage;
}

#[async_trait]
impl<MempoolAdapter, WalletAdapter, ChainService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for SdpService<MempoolAdapter, WalletAdapter, ChainService, RuntimeServiceId>
where
    MempoolAdapter: SdpMempoolAdapter<Tx = SignedMantleTx> + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData<Tx = SignedMantleTx> + Send + Sync + 'static,
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
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        Ok(Self {
            declaration_id: settings.declaration_id,
            current_declaration: None, // Will be fetched from ledger in run()
            service_resources_handle,
            nonce: 0, // Will be fetched from ledger in run()
            wallet_config: settings.wallet_config,
            _chain_service: std::marker::PhantomData,
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

        self.try_restore_declaration_state(&chain_api).await;

        self.service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        while let Some(msg) = self.service_resources_handle.inbound_relay.recv().await {
            match msg {
                SdpMessage::PostActivity { metadata, .. } => {
                    metrics::activity_posts_total();

                    self.handle_post_activity(metadata, &wallet_adapter, &mempool_adapter)
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

                    self.handle_post_withdrawal(declaration_id, &wallet_adapter, &mempool_adapter)
                        .await;
                }
            }
        }

        Ok(())
    }
}

impl<MempoolAdapter, WalletAdapter, ChainService, RuntimeServiceId>
    SdpService<MempoolAdapter, WalletAdapter, ChainService, RuntimeServiceId>
where
    MempoolAdapter: SdpMempoolAdapter<Tx = SignedMantleTx> + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData<Tx = SignedMantleTx> + Send + Sync + 'static,
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
    async fn try_restore_declaration_state(
        &mut self,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
    ) {
        let Some(declaration_id) = self.declaration_id else {
            return;
        };

        match self
            .fetch_declaration_from_ledger(chain_api, declaration_id)
            .await
        {
            Ok(Some((declaration, nonce))) => {
                tracing::info!(
                    declaration_id = ?declaration_id,
                    nonce = nonce,
                    "Loaded declaration from ledger"
                );
                self.current_declaration = Some(declaration);
                self.nonce = nonce;
            }
            Ok(None) => {
                tracing::warn!(
                    declaration_id = ?declaration_id,
                    "Declaration not found in ledger - may have been withdrawn or not yet confirmed"
                );
            }
            Err(e) => {
                tracing::error!(
                    declaration_id = ?declaration_id,
                    error = ?e,
                    "Failed to fetch declaration from ledger"
                );
            }
        }
    }

    /// Fetch declaration info from the ledger via chain service.
    async fn fetch_declaration_from_ledger(
        &self,
        chain_api: &CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
        declaration_id: DeclarationId,
    ) -> Result<Option<(RuntimeDeclaration, u64)>, DynError> {
        // Get current chain info to find the tip
        let info = chain_api.info().await?;
        let tip = info.tip;

        // Get ledger state at tip
        let Some(ledger_state) = chain_api.get_ledger_state(tip).await? else {
            return Err(format!("Ledger state not found for tip {tip:?}").into());
        };

        // Look up the declaration in the SDP ledger
        let sdp_ledger = ledger_state.mantle_ledger().sdp_ledger();
        let Some(declaration) = sdp_ledger.get_declaration(&declaration_id) else {
            return Ok(None);
        };

        Ok(Some((
            RuntimeDeclaration {
                id: declaration_id,
                zk_id: declaration.zk_id,
                locked_note_id: declaration.locked_note_id,
            },
            declaration.nonce,
        )))
    }

    async fn handle_post_declaration(
        &self,
        declaration: Box<DeclarationMessage>,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
        reply_channel: oneshot::Sender<Result<DeclarationId, DynError>>,
    ) {
        let tx_builder = MantleTxBuilder::new();
        let declaration_id = declaration.id();

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
    }

    async fn handle_post_activity(
        &mut self,
        metadata: ActivityMetadata,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
    ) {
        // Check if we have a declaration_id
        let Some(ref declaration) = self.current_declaration else {
            tracing::error!("No declaration_id set. Cannot post activity without declaration.");
            return;
        };

        let active_message = ActiveMessage {
            declaration_id: declaration.id,
            nonce: self.bump_nonce(),
            metadata,
        };

        let tx_builder = MantleTxBuilder::new();

        let signed_tx = match wallet_adapter
            .active_tx(tx_builder, active_message, &self.wallet_config)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                tracing::error!("Failed to create activity transaction: {:?}", e);
                metrics::activity_tx_failures_total();
                return;
            }
        };

        if let Err(e) = mempool_adapter.post_tx(signed_tx).await {
            tracing::error!("Failed to post activity to mempool: {:?}", e);
            metrics::activity_mempool_failures_total();
        } else {
            metrics::activity_success_total();
        }
    }

    async fn handle_post_withdrawal(
        &mut self,
        declaration_id: DeclarationId,
        wallet_adapter: &WalletAdapter,
        mempool_adapter: &MempoolAdapter,
    ) {
        if let Err(e) = self.validate_withdrawal(&declaration_id) {
            tracing::error!("{}", e);
            metrics::withdrawal_validation_failures_total();
            return;
        }

        let declaration = self.current_declaration.as_ref().unwrap(); //unwrap is ok as it is validated above
        let withdraw_message = WithdrawMessage {
            declaration_id,
            locked_note_id: declaration.locked_note_id,
            nonce: self.bump_nonce(),
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

        self.current_declaration = None;
        // TODO: how should we reset the nonce? shouldn't it be always with
        // current_delcaration?
    }

    fn validate_withdrawal(&self, declaration_id: &DeclarationId) -> Result<(), &'static str> {
        let declaration = self
            .current_declaration
            .as_ref()
            .ok_or("No declaration_id set. Cannot post withdrawal without declaration.")?;

        if *declaration_id != declaration.id {
            return Err(
                "Wrong declaration_id set. Cannot post withdrawal without proper declaration id.",
            );
        }

        Ok(())
    }

    /// Increments the nonce of the current declaration, and returns the
    /// incremented nonce.
    ///
    /// Nonce must be incremented first because it is initialized to 0 with the
    /// declaration, and each SDP message (activity or withdrawal) must have
    /// nonce larger than the previous one.
    const fn bump_nonce(&mut self) -> u64 {
        self.nonce += 1;
        self.nonce
    }
}
