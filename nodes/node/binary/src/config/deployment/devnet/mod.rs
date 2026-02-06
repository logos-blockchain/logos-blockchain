use crate::config::{
    OnUnknownKeys, deployment::DeploymentSettings, deserialize_config_from_reader,
};

pub const NAME: &str = "devnet";

#[must_use]
pub fn deployment_settings() -> DeploymentSettings {
    const SERIALIZED_DEVNET_CONFIG: &str = include_str!("config.yaml");
    deserialize_config_from_reader(SERIALIZED_DEVNET_CONFIG.as_bytes(), OnUnknownKeys::Fail)
        .expect("Well-known devnet deployment should have valid structure")
}

#[cfg(test)]
mod tests {
    use crate::config::deployment::devnet::deployment_settings;

    #[test]
    fn deployment_config_deserializes_correctly() {
        drop(deployment_settings());
    }
}
