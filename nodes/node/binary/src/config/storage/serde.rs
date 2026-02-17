use serde::{Deserialize, Serialize};

use crate::config::state::RECOVERY_FOLDER_NAME;

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
    #[serde(deserialize_with = "check_for_reserved_name_used")]
    pub folder_name: String,
    pub read_only: bool,
    pub column_family: Option<String>,
}

fn check_for_reserved_name_used<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let folder_name = String::deserialize(deserializer)?;
    if folder_name == RECOVERY_FOLDER_NAME {
        return Err(serde::de::Error::custom(format!(
            "DB folder name cannot be '{RECOVERY_FOLDER_NAME}' as that is reserved for internal usage.",
        )));
    }
    Ok(folder_name)
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

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn cannot_deserialize_reserved_name() {
        const CONFIG_STR: &str = r#"
        backend: 
            folder_name: "recovery"
        "#;

        let Err(deserialization_error) = serde_yaml::from_str::<Config>(CONFIG_STR) else {
            panic!("Deserialization should have failed due to reserved folder name");
        };
        assert!(
            deserialization_error
                .to_string()
                .contains("DB folder name cannot be 'recovery'")
        );
    }
}
