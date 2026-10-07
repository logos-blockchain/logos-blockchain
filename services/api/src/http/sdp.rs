use std::fmt::{Debug, Display};

use lb_chain_service::api::CryptarchiaServiceData;
use lb_core::sdp::{ActivityMetadata, DeclarationId, DeclarationMessage};
use lb_sdp_service::{
    SdpService, SdpSubmission, mempool::SdpMempoolAdapter, state::SdpStateStorage,
};
use overwatch::{DynError, overwatch::OverwatchHandle};

pub async fn post_declaration_handler<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    RuntimeServiceId,
>(
    handle: OverwatchHandle<RuntimeServiceId>,
    declaration: DeclarationMessage,
) -> Result<SdpSubmission, DynError>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    RuntimeServiceId: Send
        + Sync
        + Debug
        + Display
        + 'static
        + overwatch::services::AsServiceId<
            SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>,
        >,
{
    let relay = handle.relay().await?;
    let (reply_channel, reply_rx) = tokio::sync::oneshot::channel();

    relay
        .send(lb_sdp_service::SdpMessage::PostDeclaration {
            declaration: Box::new(declaration),
            reply_channel,
        })
        .await?;

    reply_rx.await?
}

pub async fn post_activity_handler<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    RuntimeServiceId,
>(
    handle: OverwatchHandle<RuntimeServiceId>,
    metadata: ActivityMetadata,
) -> Result<SdpSubmission, DynError>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    RuntimeServiceId: Send
        + Sync
        + Debug
        + Display
        + 'static
        + overwatch::services::AsServiceId<
            SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>,
        >,
{
    let relay = handle.relay().await?;
    let (reply_channel, reply_rx) = tokio::sync::oneshot::channel();

    relay
        .send(lb_sdp_service::SdpMessage::PostActivity {
            metadata,
            reply_channel: Some(reply_channel),
        })
        .await?;

    reply_rx.await?
}

pub async fn post_withdrawal_handler<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    RuntimeServiceId,
>(
    handle: OverwatchHandle<RuntimeServiceId>,
    declaration_id: DeclarationId,
) -> Result<SdpSubmission, DynError>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    RuntimeServiceId: Send
        + Sync
        + Debug
        + Display
        + 'static
        + overwatch::services::AsServiceId<
            SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>,
        >,
{
    let relay = handle.relay().await?;
    let (reply_channel, reply_rx) = tokio::sync::oneshot::channel();

    relay
        .send(lb_sdp_service::SdpMessage::PostWithdrawal {
            declaration_id,
            reply_channel,
        })
        .await?;

    reply_rx.await?
}

pub async fn post_set_declaration_id_handler<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    RuntimeServiceId,
>(
    handle: OverwatchHandle<RuntimeServiceId>,
    declaration_id: Option<DeclarationId>,
) -> Result<(), DynError>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    ChainService: CryptarchiaServiceData + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    RuntimeServiceId: Send
        + Sync
        + Debug
        + Display
        + 'static
        + overwatch::services::AsServiceId<
            SdpService<MempoolAdapter, WalletAdapter, ChainService, StateStorage, RuntimeServiceId>,
        >,
{
    let relay = handle.relay().await?;
    let (reply_channel, reply_rx) = tokio::sync::oneshot::channel();

    relay
        .send(lb_sdp_service::SdpMessage::SetCurrentDeclarationId {
            declaration_id,
            reply_channel,
        })
        .await?;

    reply_rx.await?.map_err(|e| Box::new(e) as DynError)?;

    Ok(())
}
