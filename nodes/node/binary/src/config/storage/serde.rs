use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub backend: RocksDbSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RocksDbSettings {
    /// Name of the DB state folder, relative to the state path, which is
    /// provided as a separate config entry.
    pub folder_name: String,
    pub read_only: bool,
    pub column_family: Option<String>,
}

impl Default for RocksDbSettings {
    fn default() -> Self {
        Self {
            column_family: Some("blocks".to_owned()),
            folder_name: "./db".to_owned(),
            read_only: false,
        }
    }
}
