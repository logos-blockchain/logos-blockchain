use core::{
    fmt::{self, Display, Formatter},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::config::{
    OnUnknownKeys, blend::deployment::Settings as BlendDeploymentSettings,
    cryptarchia::deployment::Settings as CryptarchiaDeploymentSettings,
    deserialize_config_from_reader, mempool::deployment::Settings as MempoolDeploymentSettings,
    network::deployment::Settings as NetworkDeploymentSettings,
    time::deployment::Settings as TimeDeploymentSettings,
};

const DEVNET: &str = "devnet";

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub enum WellKnownDeployment {
    // Must match the `DEVNET` definition above.
    #[serde(rename = "devnet")]
    #[default]
    Devnet,
}

impl FromStr for WellKnownDeployment {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            DEVNET => Ok(Self::Devnet),
            _ => Err(()),
        }
    }
}

impl Display for WellKnownDeployment {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Devnet => write!(f, "{DEVNET}"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeploymentSettings {
    pub blend: BlendDeploymentSettings,
    pub network: NetworkDeploymentSettings,
    pub cryptarchia: CryptarchiaDeploymentSettings,
    pub time: TimeDeploymentSettings,
    pub mempool: MempoolDeploymentSettings,
}

impl From<WellKnownDeployment> for DeploymentSettings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Devnet => devnet_deployment_settings(),
        }
    }
}

fn devnet_deployment_settings() -> DeploymentSettings {
    const SERIALIZED_DEVNET_CONFIG: &str =
        include_str!("../../../well-known-deployments/devnet.yaml");
    deserialize_config_from_reader(SERIALIZED_DEVNET_CONFIG.as_bytes(), OnUnknownKeys::Fail)
        .expect("Well-known devnet deployment should have valid structure")
}
