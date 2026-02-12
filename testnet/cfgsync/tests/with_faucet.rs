use std::{fs, time::Duration};

use lb_node::{
    UserConfig,
    config::{RunConfig, deployment::DeploymentSettings},
};
use lb_tests::nodes::validator::Validator;
use lb_utils::net::{get_available_tcp_port, get_available_udp_port};
use tokio::{process::Command, task::JoinSet, time::sleep};

const SERVER_BIN: &str = "../../target/debug/logos-blockchain-cfgsync-server";
const CLIENT_BIN: &str = "../../target/debug/logos-blockchain-cfgsync-client";
const FAUCET_BIN: &str = "../../target/debug/logos-blockchain-faucet";
const SERVER_CFG: &str = "./tests/cfgsync.yaml";

#[ignore = "For local debugging"]
#[tokio::test]
async fn test_spawn_nodes_and_faucet() {
    // Start the configuration server
    let mut server = std::process::Command::new(SERVER_BIN)
        .arg(SERVER_CFG)
        .spawn()
        .expect("server failed");

    sleep(Duration::from_secs(1)).await;

    // Collect all UserConfigs by running clients.
    // Genesis is only generated after all init nodes registers.
    let mut client_tasks = JoinSet::new();
    for i in 0..4 {
        let net_port = get_available_udp_port().expect("no free port for network");
        let blend_port = get_available_udp_port().expect("no free port for blend");
        let api_port = get_available_tcp_port().expect("no free port for api");
        let out = format!(".tmp_node_{i}.yaml");

        client_tasks.spawn(async move {
            let status = Command::new(CLIENT_BIN)
                .env("CFG_FILE_PATH", &out)
                .env("CFG_SERVER_ADDR", "http://127.0.0.1:4400")
                .env("CFG_HOST_IP", format!("127.0.0.{i}"))
                .env("CFG_HOST_IDENTIFIER", format!("node-{i}"))
                .env("CFG_NETWORK_PORT", net_port.to_string())
                .env("CFG_BLEND_PORT", blend_port.to_string())
                .env("CFG_API_PORT", api_port.to_string())
                .status()
                .await
                .expect("client failed");

            assert!(status.success(), "Client for node {i} failed");

            let yaml = fs::read_to_string(&out).expect("read failed");
            let user_config: UserConfig =
                serde_yaml::from_str(&yaml).expect("UserConfig parse failed");

            (i, user_config, api_port)
        });
    }

    let mut user_configs = Vec::new();
    while let Some(res) = client_tasks.join_next().await {
        user_configs.push(res.unwrap());
    }

    // Download the shared deployment config with new genesis.
    let response = reqwest::get("http://127.0.0.1:4400/deployment-settings")
        .await
        .unwrap();
    let response = response.error_for_status().unwrap();
    let yaml_bytes = response.bytes().await.unwrap();
    let deployment: DeploymentSettings = serde_yaml::from_slice(&yaml_bytes).unwrap();

    // Spawn the nodes.
    let mut nodes = Vec::new();
    for (i, user_config, api) in &user_configs {
        let run_config = RunConfig {
            user: user_config.clone(),
            deployment: deployment.clone(),
        };

        println!(">>>> Spawning Node {i} http://localhost:{api}/cryptarchia/info");
        let node = Validator::spawn(run_config).await.expect("spawn failed");
        nodes.push(node);
    }

    let mut faucet_proc = Command::new(FAUCET_BIN)
        .arg("--port")
        .arg("3000")
        .arg("--node-base-url")
        .arg(format!(
            "http://{}",
            user_configs[0].1.api.backend.listen_address
        ))
        .arg("--drip-amount")
        .arg("1000")
        .arg("--host-identifier")
        .arg("node-0")
        .spawn()
        .expect("faucet failed to start");

    // Clean up faucet on exit
    tokio::spawn(async move {
        drop(tokio::signal::ctrl_c().await);
        drop(faucet_proc.kill());
    });

    println!("\nAll nodes spawned. Use Ctrl+C to shutdown.\n");
    tokio::signal::ctrl_c().await.unwrap();

    drop(nodes);
    server.kill().unwrap();
    server.wait().unwrap();
}
