use lb_libp2p::{Multiaddr, PeerId};
use lb_network_service::{
    backends::libp2p::{
        Command, Dial, Libp2pInfo,
        NetworkCommand::{Connect, Info},
    },
    message::NetworkMsg,
};

use super::{Error, Handle, request};
use crate::NetworkService;

pub async fn info(handle: &Handle) -> Result<Libp2pInfo, Error> {
    request::<NetworkService, _>(handle, |reply| {
        NetworkMsg::Process(Command::Network(Info { reply }))
    })
    .await
}

pub async fn connect_peer(handle: &Handle, addr: Multiaddr) -> Result<PeerId, Error> {
    request::<NetworkService, _>(handle, |result_sender| {
        NetworkMsg::Process(Command::Network(Connect(Dial {
            addr,
            retry_count: 0,
            result_sender,
        })))
    })
    .await?
    .map_err(Error::internal)
}
