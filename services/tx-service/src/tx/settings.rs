use core::num::NonZeroUsize;

use lb_services_utils::overwatch::{RecoveryData, StorageRecoverySettings};
use serde::{Deserialize, Serialize};

/// The key the mempool snapshot is stored under.
///
/// `v2` since 0.3.6: the snapshot gained `included_in_canonical`, and the
/// value of `removed_items` changed from the removal time to the admission
/// time. The snapshot is bincode, which has no field defaults, so a 0.3.4
/// snapshot cannot be decoded as the new one, and a failed decode stops the
/// service from starting. Under a new key the 0.3.4 snapshot is never read:
/// an upgraded node starts with an empty mempool.
pub const RECOVERY_KEY_SUFFIX: &[u8] = b"mempool/v2";

/// Settings for the tx mempool service.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxMempoolSettings<PoolSettings, NetworkAdapterSettings> {
    /// The mempool settings.
    pub pool: PoolSettings,
    /// The network adapter settings.
    pub network_adapter: NetworkAdapterSettings,
    pub seen_items_subscription_channel_size: NonZeroUsize,
    #[serde(skip)]
    pub recovery_data: RecoveryData,
}

impl<PoolSettings, NetworkAdapterSettings> StorageRecoverySettings
    for TxMempoolSettings<PoolSettings, NetworkAdapterSettings>
{
    const RECOVERY_KEY_SUFFIX: &'static [u8] = RECOVERY_KEY_SUFFIX;

    fn recovery_data(&self) -> &RecoveryData {
        &self.recovery_data
    }
}
