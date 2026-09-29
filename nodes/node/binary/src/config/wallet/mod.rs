use lb_services_utils::overwatch::RecoveryData;
use lb_wallet_service::WalletServiceSettings;

use crate::config::wallet::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_wallet_service_settings(
        self,
        recovery_data: RecoveryData,
    ) -> WalletServiceSettings {
        // TODO(hd_wallet_05_wallet): The wallet takes the ids of its keys and the
        // first receive index that funding spends from.
        todo!()
    }
}
