use std::path::PathBuf;

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
