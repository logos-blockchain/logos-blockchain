use std::{env, fs, net::Ipv4Addr, process};

use lb_node::UserConfig as ValidatorConfig;
use logos_blockchain_cfgsync::{
    client::get_config,
    server::{ClientIp, CustomClientIp},
};
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
    let ip = parse_ip(&env::var("CFG_HOST_IP").unwrap_or_else(|_| "127.0.0.1".to_owned()));
    let identifier =
        env::var("CFG_HOST_IDENTIFIER").unwrap_or_else(|_| "unidentified-node".to_owned());

    let network_port = get_optional_u16("CFG_NETWORK_PORT");
    let blend_port = get_optional_u16("CFG_BLEND_PORT");
    let api_port = get_optional_u16("CFG_API_PORT");

    let config_result = if let (Some(np), Some(bp), Some(ap)) = (network_port, blend_port, api_port)
    {
        let endpoint = format!("{server_addr}/init/custom-node");
        let payload = CustomClientIp {
            ip,
            identifier,
            network_port: np,
            blend_port: bp,
            api_port: ap,
        };
        println!("Using custom validator endpoint with ports: {np}, {bp}, {ap}");
        pull_to_file::<ValidatorConfig, _>(&payload, &endpoint, &config_file_path).await
    } else {
        let endpoint = format!("{server_addr}/init/default-node");
        let payload = ClientIp { ip, identifier };
        pull_to_file::<ValidatorConfig, _>(&payload, &endpoint, &config_file_path).await
    };

    if let Err(err) = config_result {
        eprintln!("Error: {err}");
        process::exit(1);
    }
}
