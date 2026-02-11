use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig<BackendSettings> {
    pub backend: BackendSettings,
}
