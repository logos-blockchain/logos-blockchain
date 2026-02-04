use std::{env, fs, net::Ipv4Addr, process};

use lb_node::UserConfig as ValidatorConfig;
use logos_blockchain_cfgsync::{RegistrationInfo, client::get_config};
use serde::{Serialize, de::DeserializeOwned};

fn parse_ip(ip_str: &str) -> Ipv4Addr {
    ip_str.parse().unwrap_or_else(|_| {
        eprintln!("Invalid IP format, defaulting to 127.0.0.1");
        Ipv4Addr::LOCALHOST
    })
}

fn get_optional_u16(var_name: &str) -> Option<u16> {
    env::var(var_name).ok()?.parse().ok()
}

async fn pull_to_file<Config, Payload>(
    payload: &Payload,
    url: &str,
    config_file: &str,
) -> Result<(), String>
where
    Config: Serialize + DeserializeOwned,
    Payload: Serialize + Sync,
{
    let config = get_config::<Config, Payload>(payload, url).await?;
    let yaml = serde_yaml::to_string(&config)
        .map_err(|err| format!("Failed to serialize config to YAML: {err}"))?;

    fs::write(config_file, yaml).map_err(|err| format!("Failed to write config to file: {err}"))?;
    println!("Config saved to {config_file}");
    Ok(())
}

#[tokio::main]
async fn main() {
    let config_file_path = env::var("CFG_FILE_PATH").unwrap_or_else(|_| "config.yaml".to_owned());
    let server_addr =
        env::var("CFG_SERVER_ADDR").unwrap_or_else(|_| "http://127.0.0.1:4400".to_owned());

    let payload = RegistrationInfo {
        ip: parse_ip(&env::var("CFG_HOST_IP").unwrap_or_else(|_| "127.0.0.1".to_owned())),
        identifier: env::var("CFG_HOST_IDENTIFIER")
            .unwrap_or_else(|_| "unidentified-node".to_owned()),
        network_port: get_optional_u16("CFG_NETWORK_PORT"),
        blend_port: get_optional_u16("CFG_BLEND_PORT"),
        api_port: get_optional_u16("CFG_API_PORT"),
    };

    let endpoint = format!("{server_addr}/init-with-node");

    println!(
        "Requesting config for node '{}' at {}...",
        payload.identifier, payload.ip
    );

    if let Err(err) =
        pull_to_file::<ValidatorConfig, _>(&payload, &endpoint, &config_file_path).await
    {
        eprintln!("Error: {err}");
        process::exit(1);
    }
}
