use serde::{Deserialize, Serialize};

use crate::config::{state::RECOVERY_FOLDER_NAME, utils};

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub backend: RocksDbSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RocksDbSettings {
    /// Name of the DB state folder, relative to the state path, which is
    /// provided as a separate config entry.
    #[serde(deserialize_with = "check_for_reserved_name_used")]
    #[serde(skip_serializing_if = "is_default_folder_name")]
    pub folder_name: String,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub read_only: bool,
    #[serde(skip_serializing_if = "is_default_column_family")]
    pub column_family: String,
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

fn default_folder_name() -> String {
    "./db".to_owned()
}

fn is_default_folder_name(folder_name: &String) -> bool {
    *folder_name == default_folder_name()
}

fn default_column_family() -> String {
    "blocks".to_owned()
}

fn is_default_column_family(column_family: &String) -> bool {
    *column_family == default_column_family()
}

impl Default for RocksDbSettings {
    fn default() -> Self {
        Self {
            column_family: default_column_family(),
            folder_name: default_folder_name(),
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
