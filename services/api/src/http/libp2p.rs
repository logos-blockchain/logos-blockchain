use std::fmt::{Debug, Display};

use lb_libp2p::{Multiaddr, PeerId};
use lb_network_service::{
    NetworkService,
    backends::libp2p::{
        Command, Dial, Libp2p, Libp2pInfo,
        NetworkCommand::{Connect, Info},
    },
    message::NetworkMsg,
};
use overwatch::services::AsServiceId;
use tokio::sync::oneshot;

pub async fn libp2p_info<RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<Libp2pInfo, overwatch::DynError>
where
    RuntimeServiceId:
        AsServiceId<NetworkService<Libp2p, RuntimeServiceId>> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay().await?;
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

pub async fn connect_peer<RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    addr: Multiaddr,
) -> Result<PeerId, overwatch::DynError>
where
    RuntimeServiceId:
        AsServiceId<NetworkService<Libp2p, RuntimeServiceId>> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(NetworkMsg::Process(Command::Network(Connect(Dial {
            addr,
            retry_count: 0,
            result_sender: sender,
        }))))
        .await
        .map_err(|(e, _)| e)?;

    let dial_result = receiver
        .await
        .map_err(|e| Box::new(e) as overwatch::DynError)?;

    dial_result.map_err(|e| Box::new(e) as overwatch::DynError)
}
