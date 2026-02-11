pub mod client;
pub mod config;
pub mod repo;
pub mod server;

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

const DEFAULT_LIBP2P_NETWORK_PORT: u16 = 3000;
const DEFAULT_BLEND_PORT: u16 = 3400;
const DEFAULT_API_PORT: u16 = 18080;

#[derive(Eq, PartialEq, Hash, Clone)]
pub struct Host {
    pub ip: Ipv4Addr,
    pub identifier: String,
    pub network_port: u16,
    pub blend_port: u16,
    pub api_port: u16,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            ip: Ipv4Addr::LOCALHOST,
            identifier: String::new(),
            network_port: DEFAULT_LIBP2P_NETWORK_PORT,
            blend_port: DEFAULT_BLEND_PORT,
            api_port: DEFAULT_API_PORT,
        }
    }
}

impl From<RegistrationInfo> for Host {
    fn from(info: RegistrationInfo) -> Self {
        let mut host = Self {
            ip: info.ip,
            identifier: info.identifier,
            ..Default::default()
        };

        if let Some(p) = info.network_port {
            host.network_port = p;
        }
        if let Some(p) = info.blend_port {
            host.blend_port = p;
        }
        if let Some(p) = info.api_port {
            host.api_port = p;
        }

        host
    }
}

#[derive(Serialize, Deserialize)]
pub struct RegistrationInfo {
    pub ip: Ipv4Addr,
    pub identifier: String,
    pub network_port: Option<u16>,
    pub blend_port: Option<u16>,
    pub api_port: Option<u16>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FaucetSettings {
    pub note_count: usize,
    pub note_value: u64,
}
