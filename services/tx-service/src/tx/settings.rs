use std::path::PathBuf;

use lb_services_utils::overwatch::recovery::backends::FileBackendSettings;

/// Settings for the tx mempool service.
#[derive(Clone, Debug, Default)]
pub struct TxMempoolSettings<PoolSettings, NetworkAdapterSettings> {
    /// The mempool settings.
    pub pool: PoolSettings,
    /// The network adapter settings.
    pub network_adapter: NetworkAdapterSettings,
    /// The recovery file path, for the service's [`RecoveryOperator`].
    pub recovery_path: PathBuf,
}

impl<PoolSettings, NetworkAdapterSettings> FileBackendSettings
    for TxMempoolSettings<PoolSettings, NetworkAdapterSettings>
{
    fn recovery_file(&self) -> &PathBuf {
        &self.recovery_path
    }
}
