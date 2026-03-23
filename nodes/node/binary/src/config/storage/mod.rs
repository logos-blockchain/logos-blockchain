use lb_storage_service::backends::rocksdb::RocksBackendSettings;

use crate::config::{state::Config as StateConfig, storage::serde::Config};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_rocks_backend_settings(self, state_config: &StateConfig) -> RocksBackendSettings {
        RocksBackendSettings {
            column_family: self.user.backend.column_family,
            db_path: state_config.base_folder.join(self.user.backend.folder_name),
            read_only: self.user.backend.read_only,
        }
    }
}
