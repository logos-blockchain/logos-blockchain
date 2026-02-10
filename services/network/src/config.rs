#[derive(Debug, Clone)]
pub struct NetworkConfig<BackendSettings> {
    pub backend: BackendSettings,
}
