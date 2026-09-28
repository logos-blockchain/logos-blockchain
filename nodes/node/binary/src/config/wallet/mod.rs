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
            known_keys: self
                .user
                .known_keys
                .into_iter()
                .map(|(key_id, public_key)| (key_id.into(), public_key))
                .collect(),
            voucher_master_key_id: self.user.voucher_master_key_id.into(),
            recovery_data,
            pending_note_expiry_blocks: self.user.pending_note_expiry_blocks,
        }
    }
}
