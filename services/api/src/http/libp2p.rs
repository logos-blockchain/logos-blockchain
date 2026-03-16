use std::fmt::{Debug, Display};

use lb_banning_service::BanningService;
use lb_network_service::{
    NetworkService,
    backends::libp2p::{Command, Libp2p, Libp2pInfo, NetworkCommand::Info},
    message::NetworkMsg,
};
use overwatch::services::AsServiceId;
use tokio::sync::oneshot;

pub async fn libp2p_info<RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<Libp2pInfo, overwatch::DynError>
where
    RuntimeServiceId: AsServiceId<NetworkService<Libp2p, RuntimeServiceId>>
        + Debug
        + Sync
        + Display
        + Send
        + 'static,
    RuntimeServiceId: AsServiceId<BanningService<RuntimeServiceId>>,
{
    let relay = handle
        .relay::<NetworkService<Libp2p, RuntimeServiceId>>()
        .await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(NetworkMsg::Process(Command::Network(Info {
            reply: sender,
        })))
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|e| Box::new(e) as overwatch::DynError)
}
