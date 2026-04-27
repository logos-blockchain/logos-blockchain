pub mod client;
pub mod config;
pub mod repo;
pub mod server;

use std::{net::Ipv4Addr, path::Path};

use blake2::{Blake2b, Digest as _, digest::consts::U32};
use clap::ValueEnum;
use rand::Rng as _;
use serde::{Deserialize, Serialize};

pub type Entropy = [u8; 32];

/// Load entropy from a file, hashing the contents with blake2b-256 to normalize
/// to 32 bytes.
pub fn load_entropy(path: &Path) -> Result<Entropy, String> {
    let data = std::fs::read(path)
        .map_err(|e| format!("Failed to read entropy file {}: {e}", path.display()))?;
    let hash = Blake2b::<U32>::digest(&data);
    Ok(hash.into())
}

/// Generate random entropy bytes.
#[must_use]
pub fn random_entropy() -> Entropy {
    let mut rng = rand::thread_rng();
    let mut entropy: Entropy = [0u8; 32];
    rng.fill(&mut entropy);
    entropy
}

const DEFAULT_LIBP2P_NETWORK_PORT: u16 = 3000;
const DEFAULT_BLEND_PORT: u16 = 3400;
const DEFAULT_API_PORT: u16 = 18080;

#[derive(Eq, PartialEq, PartialOrd, Ord, Hash, Clone)]
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
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum CfgsyncMode {
    Setup,
    Run,
}
