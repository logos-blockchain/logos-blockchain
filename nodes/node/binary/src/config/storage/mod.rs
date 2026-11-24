use lb_storage_service::rocksdb::RocksBackendSettings;

use crate::config::{state::Config as StateConfig, storage::serde::Config};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_rocks_backend_settings(self, state_config: &StateConfig) -> RocksBackendSettings {
        let backend = self.user.backend;
        RocksBackendSettings {
            db_path: backend.db_path(&state_config.base_folder),
            column_family: backend.column_family,
            read_only: backend.read_only,
        }
    }
}
