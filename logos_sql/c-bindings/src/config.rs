use std::path::PathBuf;

use logos_sql::LogosSqlConfig;
use serde::Deserialize;

use crate::SqlStatus;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    channel_id: String,
    node_url: String,
    state_dir: PathBuf,
}

impl Config {
    pub(crate) fn parse(json: &str) -> Result<LogosSqlConfig, SqlStatus> {
        let config: Self = serde_json::from_str(json)
            .map_err(|error| SqlStatus::invalid(format!("Invalid configuration JSON: {error}")))?;
        let node_url = config
            .node_url
            .parse()
            .map_err(|error| SqlStatus::invalid(format!("Invalid node_url: {error}")))?;

        Ok(LogosSqlConfig {
            channel_id: hex_array(&config.channel_id)?.into(),
            node_url,
            state_dir: config.state_dir,
            writer: None,
        })
    }
}

fn hex_array<const N: usize>(value: &str) -> Result<[u8; N], SqlStatus> {
    let mut bytes = [0; N];
    hex::decode_to_slice(value, &mut bytes)
        .map_err(|error| SqlStatus::invalid(error.to_string()))?;
    Ok(bytes)
}
