use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Config {
    pub base_folder: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_folder: "./state".into(),
        }
    }
}

impl Config {
    #[must_use]
    pub fn get_path_for_recovery_path(&self, recovery_path: &Path) -> PathBuf {
        self.base_folder.join("recovery").join(recovery_path)
    }
}
