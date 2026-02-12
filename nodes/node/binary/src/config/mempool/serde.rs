use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub recovery_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            recovery_path: "./mempool_recovery.json".into(),
        }
    }
}
