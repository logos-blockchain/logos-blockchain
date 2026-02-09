use lb_libp2p::protocol_name::StreamProtocol;
use serde::{Deserialize, Serialize};

use crate::config::deployment::WellKnownDeployment;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub kademlia_protocol_name: StreamProtocol,
    pub identify_protocol_name: StreamProtocol,
    pub chain_sync_protocol_name: StreamProtocol,
}

impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Devnet => devnet_settings(),
        }
    }
}

const fn devnet_settings() -> Settings {
    Settings {
        chain_sync_protocol_name: StreamProtocol::new("/logos-blockchain-devnet/chainsync/1.0.0"),
        identify_protocol_name: StreamProtocol::new("/logos-blockchain-devnet/identify/1.0.0"),
        kademlia_protocol_name: StreamProtocol::new("/logos-blockchain-devnet/kad/1.0.0"),
    }
}
