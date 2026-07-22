use std::fmt::{Debug, Display};

use lb_blend_service::message::{NetworkInfo, ProxyServiceMessage, ServiceMessage};
use lb_network_service::backends::libp2p::PeerId;
use overwatch::services::{AsServiceId, ServiceData};
use tokio::sync::oneshot;

pub async fn blend_info<BlendService, BroadcastSettings, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<Option<NetworkInfo<PeerId>>, overwatch::DynError>
where
    BlendService:
        ServiceData<Message = ProxyServiceMessage<ServiceMessage<BroadcastSettings, PeerId>>>,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
    BroadcastSettings: Send + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ServiceMessage::GetNetworkInfo { reply: sender }.into())
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|e| Box::new(e) as overwatch::DynError)
}

pub async fn blend_join_network<BlendService, BroadcastSettings, RuntimeServiceId>(
    handle: &overwatch::overwatch::OverwatchHandle<RuntimeServiceId>,
    locator: lb_core::sdp::Locator,
    locked_note_id: lb_core::mantle::NoteId,
) -> Result<lb_core::sdp::DeclarationId, overwatch::DynError>
where
    BlendService:
        ServiceData<Message = ProxyServiceMessage<ServiceMessage<BroadcastSettings, PeerId>>>,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
    BroadcastSettings: Send + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ProxyServiceMessage::JoinAsCore {
            locator,
            locked_note_id,
            reply: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    let result = receiver
        .await
        .map_err(|e| Box::new(e) as overwatch::DynError)??;

    Ok(result)
}
