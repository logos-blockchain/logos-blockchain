use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub backend: RocksDbSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RocksDbSettings {
    pub path: PathBuf,
    pub read_only: bool,
    pub column_family: Option<String>,
}

impl Default for RocksDbSettings {
    fn default() -> Self {
        Self {
            column_family: Some("blocks".to_owned()),
            path: "./db".into(),
            read_only: false,
        }
    }
}
