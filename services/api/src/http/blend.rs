use std::fmt::{Debug, Display};

use lb_blend_service::message::{BlendNetworkInfo, ServiceMessage};
use overwatch::services::{AsServiceId, ServiceData};
use tokio::sync::oneshot;

pub async fn blend_info<S, BroadcastSettings, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<Option<BlendNetworkInfo>, overwatch::DynError>
where
    S: ServiceData<Message = ServiceMessage<BroadcastSettings>>,
    RuntimeServiceId: AsServiceId<S> + Debug + Sync + Display + 'static,
    BroadcastSettings: Send + 'static,
{
    let relay = handle.relay::<S>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ServiceMessage::NetworkInfo { reply: sender })
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|e| Box::new(e) as overwatch::DynError)
}
