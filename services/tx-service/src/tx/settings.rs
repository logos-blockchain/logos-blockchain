use lb_cryptarchia_engine::era::EraSchedule;
use lb_services_utils::overwatch::{RecoveryData, StorageRecoverySettings};

pub const RECOVERY_KEY_SUFFIX: &[u8] = b"mempool";

/// Settings for the tx mempool service.
#[derive(Clone, Debug)]
pub struct TxMempoolSettings<PoolSettings, NetworkAdapterSettings> {
    /// The mempool settings.
    pub pool: PoolSettings,
    /// The network adapter settings of every era: each era in force has an
    /// adapter of its own, to its topic.
    pub network_adapters: EraSchedule<NetworkAdapterSettings>,
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
