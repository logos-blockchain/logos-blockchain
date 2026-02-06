pub mod mempool;
pub mod wallet;

use std::{
    collections::BTreeSet,
    fmt::{Debug, Display},
    pin::Pin,
};

use async_trait::async_trait;
use futures::Stream;
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
    /// Declaration info for this node (set after posting declaration and
    /// restarting).
    pub declaration: Option<Declaration>,
    pub wallet_config: SdpWalletConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Declaration {
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

pub struct SdpService<MempoolAdapter, WalletAdapter, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    current_declaration: Option<Declaration>,
    nonce: u64,
    wallet_config: SdpWalletConfig,
}

impl<MempoolAdapter, WalletAdapter, RuntimeServiceId> ServiceData
    for SdpService<MempoolAdapter, WalletAdapter, RuntimeServiceId>
{
    type Settings = SdpSettings;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = SdpMessage;
}

#[async_trait]
impl<MempoolAdapter, WalletAdapter, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for SdpService<MempoolAdapter, WalletAdapter, RuntimeServiceId>
where
    MempoolAdapter: SdpMempoolAdapter<Tx = SignedMantleTx> + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + AsServiceId<Self>
        + AsServiceId<MempoolAdapter::MempoolService>
        + AsServiceId<WalletAdapter::WalletService>
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
            current_declaration: settings.declaration,
            service_resources_handle,
            nonce: 0,
            wallet_config: settings.wallet_config,
        })
    }

    async fn run(mut self) -> Result<(), DynError> {
        self.service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

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

        while let Some(msg) = self.service_resources_handle.inbound_relay.recv().await {
            match msg {
                SdpMessage::PostActivity { metadata, .. } => {
                    self.handle_post_activity(metadata, &wallet_adapter, &mempool_adapter)
                        .await;
                }
                SdpMessage::PostDeclaration {
                    declaration,
                    reply_channel,
                } => {
                    self.handle_post_declaration(
                        declaration,
                        &wallet_adapter,
                        &mempool_adapter,
                        reply_channel,
                    )
                    .await;
                }
                SdpMessage::PostWithdrawal { declaration_id } => {
                    self.handle_post_withdrawal(declaration_id, &wallet_adapter, &mempool_adapter)
                        .await;
                }
            }
        }

        Ok(())
    }
}

impl<MempoolAdapter, WalletAdapter, RuntimeServiceId>
    SdpService<MempoolAdapter, WalletAdapter, RuntimeServiceId>
where
    MempoolAdapter: SdpMempoolAdapter<Tx = SignedMantleTx> + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + AsServiceId<Self>
        + AsServiceId<MempoolAdapter::MempoolService>
        + Clone
        + Display
        + Send
        + Sync
        + 'static,
{
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
                return;
            }
        };

        if let Err(e) = mempool_adapter.post_tx(signed_tx).await {
            tracing::error!("Failed to post declaration to mempool: {:?}", e);
            return;
        }

        if let Err(e) = reply_channel.send(Ok(declaration_id)) {
            tracing::error!("Failed to send post declaration response: {:?}", e);
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

        let nonce = self.nonce;
        self.nonce += 1;

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
                tracing::error!("Failed to create activity transaction: {:?}", e);
                return;
            }
        };

        if let Err(e) = mempool_adapter.post_tx(signed_tx).await {
            tracing::error!("Failed to post activity to mempool: {:?}", e);
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
            return;
        }

        let nonce = self.nonce;
        self.nonce += 1;

        let declaration = self.current_declaration.as_ref().unwrap(); //unwrap is ok as it is validated above
        let withdraw_message = WithdrawMessage {
            declaration_id,
            nonce,
            locked_note_id: declaration.locked_note_id,
        };

        let tx_builder = MantleTxBuilder::new();

        let signed_tx = match wallet_adapter
            .withdraw_tx(tx_builder, withdraw_message, &self.wallet_config)
            .await
        {
            Ok(tx) => tx,
            Err(e) => {
                tracing::error!("Failed to create withdrawal transaction: {:?}", e);
                return;
            }
        };

        if let Err(e) = mempool_adapter.post_tx(signed_tx).await {
            tracing::error!("Failed to post withdrawal to mempool: {:?}", e);
            return;
        }

        self.current_declaration = None;
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
}
