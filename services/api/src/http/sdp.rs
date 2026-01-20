use std::fmt::{Debug, Display};

use lb_core::sdp::{ActivityMetadata, DeclarationId, DeclarationMessage};
use lb_sdp_service::{SdpService, adapters::mempool::SdpMempoolAdapter};
use overwatch::{DynError, overwatch::OverwatchHandle};

pub async fn post_declaration_handler<MempoolAdapter, RuntimeServiceId>(
    handle: OverwatchHandle<RuntimeServiceId>,
    declaration: DeclarationMessage,
) -> Result<DeclarationId, DynError>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    RuntimeServiceId: Send
        + Sync
        + Debug
        + Display
        + 'static
        + overwatch::services::AsServiceId<SdpService<MempoolAdapter, RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

    relay
        .send(lb_sdp_service::SdpMessage::PostDeclaration {
            declaration: Box::new(declaration),
            reply_channel: reply_tx,
        })
        .await
        .map_err(|(e, _)| e)?;

    reply_rx.await?
}

pub async fn post_activity_handler<MempoolAdapter, RuntimeServiceId>(
    handle: OverwatchHandle<RuntimeServiceId>,
    metadata: ActivityMetadata,
) -> Result<(), DynError>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    RuntimeServiceId: Send
        + Sync
        + Debug
        + Display
        + 'static
        + overwatch::services::AsServiceId<SdpService<MempoolAdapter, RuntimeServiceId>>,
{
    let relay = handle.relay().await?;

    relay
        .send(lb_sdp_service::SdpMessage::PostActivity { metadata })
        .await
        .map_err(|(e, _)| e)?;

    Ok(())
}

pub async fn post_withdrawal_handler<MempoolAdapter, RuntimeServiceId>(
    handle: OverwatchHandle<RuntimeServiceId>,
    declaration_id: DeclarationId,
) -> Result<(), DynError>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    RuntimeServiceId: Send
        + Sync
        + Debug
        + Display
        + 'static
        + overwatch::services::AsServiceId<SdpService<MempoolAdapter, RuntimeServiceId>>,
{
    let relay = handle.relay().await?;

    relay
        .send(lb_sdp_service::SdpMessage::PostWithdrawal { declaration_id })
        .await
        .map_err(|(e, _)| e)?;

    Ok(())
}
