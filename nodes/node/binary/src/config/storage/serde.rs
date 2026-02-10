use std::path::PathBuf;

use lb_storage_service::backends::rocksdb::RocksBackendSettings;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: RocksDbSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RocksDbSettings {
    pub path: PathBuf,
    pub read_only: bool,
    pub column_family: Option<String>,
}

impl From<RocksDbSettings> for RocksBackendSettings {
    fn from(value: RocksDbSettings) -> Self {
        Self {
            column_family: value.column_family,
            db_path: value.path,
            read_only: value.read_only,
        }
    }
}

impl From<RocksBackendSettings> for RocksDbSettings {
    fn from(value: RocksBackendSettings) -> Self {
        Self {
            column_family: value.column_family,
            path: value.db_path,
            read_only: value.read_only,
        }
    }
}
