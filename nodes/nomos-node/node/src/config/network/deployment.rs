use nomos_libp2p::protocol_name::StreamProtocol;
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
            WellKnownDeployment::Mainnet => Self {
                identify_protocol_name: StreamProtocol::new("/nomos/identify/1.0.0"),
                kademlia_protocol_name: StreamProtocol::new("/nomos/kad/1.0.0"),
                chain_sync_protocol_name: StreamProtocol::new("/nomos/cryptarchia/sync/1.0.0"),
            },
        }
    }
}
