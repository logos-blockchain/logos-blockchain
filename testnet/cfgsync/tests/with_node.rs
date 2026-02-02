use std::{fs, time::Duration};

use lb_node::{UserConfig, config::RunConfig};
use lb_tests::{
    nodes::validator::Validator, topology::configs::deployment::default_e2e_deployment_settings,
};
use lb_utils::net::{get_available_tcp_port, get_available_udp_port};
use tokio::{process::Command, task::JoinSet, time::sleep};

const SERVER_BIN: &str = "../../target/debug/logos-blockchain-cfgsync-server";
const CLIENT_BIN: &str = "../../target/debug/logos-blockchain-cfgsync-client";
const SERVER_CFG: &str = "./tests/cfgsync.yaml";

#[ignore = "For local debugging"]
#[tokio::test]
async fn test_spawn_nodes_from_cfgsync_custom_ports() {
    let mut server = std::process::Command::new(SERVER_BIN)
        .arg(SERVER_CFG)
        .spawn()
        .expect("server failed");

    sleep(Duration::from_secs(1)).await;

    let mut set = JoinSet::new();
    for i in 0..4 {
        let net_port = get_available_udp_port().expect("no free port for network");
        let blend_port = get_available_udp_port().expect("no free port for blend");
        let api_port = get_available_tcp_port().expect("no free port for api");

        set.spawn(async move {
            let out = format!(".tmp_node_{i}.yaml");

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

            assert!(status.success());

            let yaml = fs::read_to_string(&out).expect("read failed");
            let user_config: UserConfig =
                serde_yaml::from_str(&yaml).expect("UserConfig parse failed");

            let run_config = RunConfig {
                user: user_config,
                deployment: default_e2e_deployment_settings(),
            };

            println!(
                ">>>> Spawning Node {i} (Net: {net_port}, Blend: {blend_port}, API: http://localhost:{api_port}/cryptarchia/info)..."
            );
            let _node = Validator::spawn(run_config).await.expect("spawn failed");

            loop {
                sleep(Duration::from_secs(3600)).await;
            }
        });
    }

    println!("\nNodes live with custom ports. Use Ctrl+C to shutdown.\n");
    tokio::signal::ctrl_c().await.unwrap();

    server.kill().unwrap();
    server.wait().unwrap();
}
