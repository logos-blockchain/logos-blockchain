use lb_storage_service::backends::rocksdb::RocksBackendSettings;

use crate::config::storage::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for RocksBackendSettings {
    fn from(value: ServiceConfig) -> Self {
        Self {
            column_family: value.user.backend.column_family,
            db_path: value.user.backend.path,
            read_only: value.user.backend.read_only,
        }
    }
}
