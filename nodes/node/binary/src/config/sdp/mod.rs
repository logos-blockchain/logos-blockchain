use lb_sdp_service::{Declaration, SdpSettings, wallet::SdpWalletConfig};

use crate::config::sdp::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for SdpSettings {
    fn from(value: ServiceConfig) -> Self {
        Self {
            declaration: value.user.declaration.map(|d| Declaration {
                id: d.id,
                zk_id: d.zk_id,
                locked_note_id: d.locked_note_id,
            }),
            wallet_config: SdpWalletConfig {
                funding_pk: value.user.wallet.funding_pk,
                max_tx_fee: value.user.wallet.max_tx_fee,
            },
        }
    }
}
