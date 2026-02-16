use std::{fs, time::Duration};

use tokio::{process::Command, task::JoinSet, time::sleep};

const SERVER_BIN: &str = "../../target/debug/logos-blockchain-cfgsync-server";
const CLIENT_BIN: &str = "../../target/debug/logos-blockchain-cfgsync-client";
const SERVER_CFG: &str = "../cfgsync.yaml"; // Use config from "testnet" dir.

#[tokio::test]
async fn smoke_test_four_clients() {
    let mut server = std::process::Command::new(SERVER_BIN)
        .arg("--mode")
        .arg("setup")
        .arg(SERVER_CFG)
        .spawn()
        .expect("server failed");

    sleep(Duration::from_secs(1)).await;

    let mut set = JoinSet::new();
    for i in 0..4 {
        set.spawn(async move {
            let out = format!(".tmp_out_{i}.yaml");
            let status = Command::new(CLIENT_BIN)
                .env("CFG_FILE_PATH", &out)
                .env("CFG_SERVER_ADDR", "http://127.0.0.1:4400")
                .env("CFG_HOST_IP", format!("127.0.0.{i}"))
                .env("CFG_HOST_IDENTIFIER", format!("node-{i}"))
                .status()
                .await
                .expect("client failed");

            (status.success(), out)
        });
    }

    while let Some(Ok((success, out))) = set.join_next().await {
        assert!(success);
        assert!(fs::metadata(&out).is_ok());
        fs::remove_file(out).unwrap();
    }

    server.kill().and_then(|()| server.wait()).unwrap();
}
