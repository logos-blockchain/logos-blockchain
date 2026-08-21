use core::time::Duration;

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
        slot_duration: Duration,
    ) -> WalletServiceSettings {
        let slot_duration_ms = u64::try_from(slot_duration.as_millis())
            .expect("configured slot duration must fit in milliseconds");
        WalletServiceSettings {
            known_keys: self.user.known_keys,
            voucher_master_key_id: self.user.voucher_master_key_id,
            recovery_data,
            pending_note_expiry_blocks: self.user.pending_note_expiry_blocks,
            slot_duration_ms,
        }
    }
}
