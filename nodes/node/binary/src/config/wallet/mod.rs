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
        WalletServiceSettings {
            known_keys: todo!("change to the set of key ids"),
            voucher_master_key_id: todo!("remove this and add funding_start_index"),
            recovery_data,
            pending_note_expiry_blocks: self.user.pending_note_expiry_blocks,
        }
    }
}
