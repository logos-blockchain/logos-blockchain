use lb_core::mantle::Value;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_sdp_service::{Declaration, SdpSettings, wallet::SdpWalletConfig};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub declaration: Option<Declaration>,
    pub wallet: WalletConfig,
}

impl From<Config> for SdpSettings {
    fn from(value: Config) -> Self {
        Self {
            declaration: value.declaration,
            wallet_config: value.wallet.into(),
        }
    }
}

impl From<SdpSettings> for Config {
    fn from(value: SdpSettings) -> Self {
        Self {
            declaration: value.declaration,
            wallet: value.wallet_config.into(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WalletConfig {
    pub max_tx_fee: Value,
    pub funding_pk: ZkPublicKey,
}

impl From<WalletConfig> for SdpWalletConfig {
    fn from(value: WalletConfig) -> Self {
        Self {
            max_tx_fee: value.max_tx_fee,
            funding_pk: value.funding_pk,
        }
    }
}

impl From<SdpWalletConfig> for WalletConfig {
    fn from(value: SdpWalletConfig) -> Self {
        Self {
            max_tx_fee: value.max_tx_fee,
            funding_pk: value.funding_pk,
        }
    }
}
