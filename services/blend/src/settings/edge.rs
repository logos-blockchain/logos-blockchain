use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EdgeSettings<BackendSettings> {
    pub backend: BackendSettings,
}
