use std::path::PathBuf;

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
