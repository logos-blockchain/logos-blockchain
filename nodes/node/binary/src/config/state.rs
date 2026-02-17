use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const RECOVERY_FOLDER_NAME: &str = "recovery";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "is_default_base_folder")]
    pub base_folder: PathBuf,
}

fn default_base_folder() -> PathBuf {
    PathBuf::from("./state")
}

fn is_default_base_folder(path: &PathBuf) -> bool {
    path == &default_base_folder()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_folder: default_base_folder(),
        }
    }
}

impl Config {
    #[must_use]
    pub fn get_path_for_recovery_state(&self, recovery_path: &Path) -> PathBuf {
        self.base_folder
            .join(RECOVERY_FOLDER_NAME)
            .join(recovery_path)
    }
}
