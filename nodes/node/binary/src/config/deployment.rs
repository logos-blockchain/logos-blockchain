use core::{
    fmt::{self, Display, Formatter},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::config::{
    blend::deployment::Settings as BlendDeploymentSettings,
    cryptarchia::deployment::Settings as CryptarchiaDeploymentSettings,
    mempool::deployment::Settings as MempoolDeploymentSettings,
    network::deployment::Settings as NetworkDeploymentSettings,
    time::deployment::Settings as TimeDeploymentSettings,
};

const MAINNET: &str = "mainnet";
const TESTNET: &str = "testnet";

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub enum WellKnownDeployment {
    // Must match the `MAINNET` definition above.
    #[serde(rename = "mainnet")]
    Mainnet,
    // Must match the `TESTNET` definition above.
    #[serde(rename = "testnet")]
    #[default]
    Testnet,
}

impl FromStr for WellKnownDeployment {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            MAINNET => Ok(Self::Mainnet),
            TESTNET => Ok(Self::Testnet),
            _ => Err(()),
        }
    }
}

impl Display for WellKnownDeployment {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mainnet => write!(f, "{MAINNET}"),
            Self::Testnet => write!(f, "{TESTNET}"),
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
        Self {
            blend: value.into(),
            cryptarchia: value.into(),
            network: value.into(),
            time: value.into(),
            mempool: value.into(),
        }
    }
}
