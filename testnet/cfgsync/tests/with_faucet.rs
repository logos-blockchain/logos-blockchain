use std::{fs, time::Duration};

use lb_groth16::fr_to_bytes;
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
        .arg("--mode")
        .arg("setup")
        .arg("--entropy-file")
        .arg("tests/test_entropy")
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

    // All nodes have the faucet SK, so route to any node.
    let faucet_pk = deployment
        .cryptarchia
        .faucet_pk
        .expect("faucet PK should be set");
    let faucet_pk_hex = hex::encode(fr_to_bytes(faucet_pk.as_fr()));

    let faucet_port = get_available_tcp_port().expect("no free port for faucet");
    let mut faucet_proc = Command::new(FAUCET_BIN)
        .arg("--port")
        .arg(faucet_port.to_string())
        .arg("--node-base-url")
        .arg(format!(
            "http://{}",
            user_configs[0].1.api.backend.listen_address
        ))
        .arg("--drip-amount")
        .arg("1000")
        .arg("--faucet-pk")
        .arg(&faucet_pk_hex)
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

#[ignore = "End-to-end deterministic faucet verification"]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "e2e test with ceremony, node, and faucet phases"
)]
async fn test_deterministic_faucet_e2e() {
    // --- Phase 1: Ceremony ---
    let mut server = std::process::Command::new(SERVER_BIN)
        .arg("--mode")
        .arg("setup")
        .arg("--entropy-file")
        .arg("tests/test_entropy")
        .arg(SERVER_CFG)
        .spawn()
        .expect("server failed");

    sleep(Duration::from_secs(1)).await;

    let mut client_tasks = JoinSet::new();
    for i in 0..4 {
        let net_port = get_available_udp_port().expect("no free port for network");
        let blend_port = get_available_udp_port().expect("no free port for blend");
        let api_port = get_available_tcp_port().expect("no free port for api");
        let out = format!(".tmp_e2e_node_{i}.yaml");

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

    let response = reqwest::get("http://127.0.0.1:4400/deployment-settings")
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let yaml_bytes = response.bytes().await.unwrap();
    let deployment: DeploymentSettings = serde_yaml::from_slice(&yaml_bytes).unwrap();

    // Verify faucet PK is in deployment settings
    let faucet_pk = deployment
        .cryptarchia
        .faucet_pk
        .expect("faucet PK should be set in genesis");
    let faucet_pk_hex = hex::encode(fr_to_bytes(faucet_pk.as_fr()));
    println!("Faucet PK: {faucet_pk_hex}");

    // Verify faucet SK is in all node configs (via known_keys values)
    for (i, user_config, _) in &user_configs {
        let has_faucet_key = user_config
            .wallet
            .known_keys
            .values()
            .any(|pk| *pk == faucet_pk);
        assert!(
            has_faucet_key,
            "Node {i} should have faucet PK in known_keys"
        );
    }
    println!("All nodes have faucet key in known_keys");

    // --- Phase 2: Spawn a single node ---
    let (_, node0_config, _) = user_configs
        .iter()
        .find(|(i, _, _)| *i == 0)
        .expect("node 0 should exist");
    let api_base = format!("http://{}", node0_config.api.backend.listen_address);
    let run_config = RunConfig {
        user: node0_config.clone(),
        deployment: deployment.clone(),
    };
    println!(">>>> Spawning Node 0 {api_base}/cryptarchia/info");
    let node = Validator::spawn(run_config).await.expect("spawn failed");
    println!("Waiting for node to start producing blocks...");
    for attempt in 0..60 {
        sleep(Duration::from_secs(2)).await;
        let url = format!("{api_base}/cryptarchia/info");
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(body) = resp.text().await
        {
            if attempt % 5 == 0 {
                println!("  attempt {attempt}: {body}");
            }
            if let Ok(info) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(height) = info["height"].as_u64()
                && height > 0
            {
                println!("Node is producing blocks (height={height})");
                break;
            }
        }
    }

    // --- Phase 3: Start faucet and drip ---
    let faucet_port = get_available_tcp_port().expect("no free port for faucet");
    let mut faucet_proc = std::process::Command::new(FAUCET_BIN)
        .arg("--port")
        .arg(faucet_port.to_string())
        .arg("--node-base-url")
        .arg(&api_base)
        .arg("--drip-amount")
        .arg("1000")
        .arg("--faucet-pk")
        .arg(&faucet_pk_hex)
        .spawn()
        .expect("faucet failed to start");

    sleep(Duration::from_secs(2)).await;

    // Pick a recipient key — use node 0's voucher_master_key_id (the leader/known
    // key)
    let recipient_pk_hex = &node0_config.wallet.voucher_master_key_id;
    println!("Dripping tokens to recipient (voucher_master_key): {recipient_pk_hex}");

    // Check initial balance
    let balance_url = format!("{api_base}/wallet/{recipient_pk_hex}/balance");
    let initial_balance = reqwest::get(&balance_url)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    println!("Initial balance response: {initial_balance}");

    // Drip tokens via faucet
    let drip_url = format!("http://127.0.0.1:{faucet_port}/faucet/{recipient_pk_hex}");
    let client = reqwest::Client::new();
    let drip_resp = client.post(&drip_url).send().await.unwrap();
    let drip_status = drip_resp.status();
    let drip_body = drip_resp.text().await.unwrap();
    println!("Faucet drip response ({drip_status}): {drip_body}");
    assert!(
        drip_status.is_success(),
        "Faucet drip should succeed: {drip_body}"
    );

    // Wait for the transaction to be included in a block
    println!("Waiting for transaction to be included...");
    sleep(Duration::from_secs(30)).await;

    // Check balance after drip
    let final_balance = reqwest::get(&balance_url)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    println!("Final balance response: {final_balance}");

    // Parse and verify balance increased
    if let Ok(balance_json) = serde_json::from_str::<serde_json::Value>(&final_balance)
        && let Some(balance) = balance_json["balance"].as_u64()
    {
        println!("Balance after drip: {balance}");
        // The recipient already had 100000 from genesis (leader note), plus 1000 from
        // drip
        assert!(balance > 0, "Balance should be > 0 after drip");
    }

    // Cleanup
    drop(faucet_proc.kill());
    drop(faucet_proc.wait());
    drop(node);
    server.kill().unwrap();
    server.wait().unwrap();

    println!("\nEnd-to-end test completed successfully!");
}
