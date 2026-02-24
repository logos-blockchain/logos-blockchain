use std::{fs, path::PathBuf, sync::Arc};

use clap::Parser;
use lb_groth16::fr_from_bytes;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_node::config::DeploymentSettings;
use logos_blockchain_faucet::{faucet::Faucet, server::faucet_app};
use reqwest::Url;
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(about = "Faucet")]
struct Args {
    #[arg(short, long, default_value_t = 6000)]
    port: u16,
    #[arg(short, long)]
    node_base_url: Url,
    /// Path to the deployment YAML file containing the faucet public key.
    #[arg(short, long, conflicts_with = "faucet_pk")]
    deployment_file: Option<PathBuf>,
    /// Hex-encoded faucet public key.
    #[arg(long, conflicts_with = "deployment_file")]
    faucet_pk: Option<String>,
    #[arg(short, long)]
    drip_amount: u64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let faucet_pk = if let Some(pk_hex) = args.faucet_pk {
        parse_pk(&pk_hex)
    } else {
        let path = args
            .deployment_file
            .expect("faucet_pk or deployment file is set");
        let yaml_bytes = fs::read(&path).expect("Could not read config file");
        let deployment: DeploymentSettings =
            serde_yaml::from_slice(&yaml_bytes).expect("Invalid YAML");

        deployment
            .cryptarchia
            .faucet_pk
            .expect("faucet_pk missing in deployment config")
    };

    println!("Faucet PK: {faucet_pk:?}");

    let faucet = Arc::new(
        Faucet::new(args.node_base_url, faucet_pk, args.drip_amount)
            .expect("faucet should be created"),
    );

    let app = faucet_app(faucet);

    println!("Faucet server running on http://0.0.0.0:{}", args.port);
    let listener = TcpListener::bind(&format!("0.0.0.0:{}", args.port))
        .await
        .unwrap();

    axum::serve(listener, app).await.unwrap();
}

fn parse_pk(hex_str: &str) -> ZkPublicKey {
    let pk_bytes = hex::decode(hex_str).expect("faucet-pk must be valid hex");
    ZkPublicKey::new(fr_from_bytes(&pk_bytes).expect("faucet-pk must be a valid field element"))
}
