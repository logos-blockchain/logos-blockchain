use std::{sync::Arc, time::Duration};

use lb_faucet::{
    faucet::{Faucet, run_worker},
    server::{FaucetServerState, faucet_app},
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::{
    NodeHttpClient,
    configs::wallet::{WalletAccount, WalletConfig},
};
use logos_blockchain_tests::common::manual_cluster::{
    api_url, get_wallet_balance, start_fast_cluster_with_wallet, wait_for_nodes_height,
};
use reqwest::StatusCode;
use serial_test::serial;
use tokio::{net::TcpListener, sync::mpsc, time::sleep};

const FAUCET_FUNDS: u64 = 2000;
const DRIP_AMOUNT: u64 = 50;
const DRIP_QUEUE_CAPACITY: usize = 16;
const COOLDOWN: Duration = Duration::from_hours(1);
const BALANCE_TIMEOUT: Duration = Duration::from_mins(3);
const BALANCE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// End-to-end test for the faucet drip queue:
///
/// 1. Spawn validators with a single-note faucet wallet.
/// 2. Run the faucet (server + worker) in-process against a validator.
/// 3. Submit two concurrent drip requests for different recipients.
/// 4. Verify both are accepted and an immediate repeat request is rejected with
///    the cooldown status.
/// 5. Verify both recipients are credited on chain, one drip per block, and the
///    faucet balance decreases by exactly two drips.
#[tokio::test]
#[serial]
async fn faucet_drips_concurrent_requests_in_order() {
    let faucet_account = WalletAccount::deterministic(0, FAUCET_FUNDS, false).expect("valid");
    let recipient_a = WalletAccount::deterministic(1, 1, false).expect("valid");
    let recipient_b = WalletAccount::deterministic(2, 2, false).expect("valid");

    let wallet_config = WalletConfig::new(vec![
        faucet_account.clone(),
        recipient_a.clone(),
        recipient_b.clone(),
    ]);

    let (_base, nodes) =
        start_fast_cluster_with_wallet("faucet-drip-queue", "mantle-faucet", 2, wallet_config)
            .await;

    let validator = &nodes[0];
    wait_for_nodes_height(&[&validator.client], 3, Duration::from_mins(5)).await;

    let faucet_url = spawn_faucet(&validator.client, faucet_account.public_key()).await;
    let client = reqwest::Client::new();

    let (resp_a, resp_b) = tokio::join!(
        client
            .post(format!(
                "{faucet_url}/faucet/{}",
                recipient_a.public_key_hex()
            ))
            .send(),
        client
            .post(format!(
                "{faucet_url}/faucet/{}",
                recipient_b.public_key_hex()
            ))
            .send(),
    );

    let resp_a = resp_a.expect("drip request for A should not fail");
    let resp_b = resp_b.expect("drip request for B should not fail");
    assert_eq!(resp_a.status(), StatusCode::ACCEPTED);
    assert_eq!(resp_b.status(), StatusCode::ACCEPTED);

    let repeat = client
        .post(format!(
            "{faucet_url}/faucet/{}",
            recipient_a.public_key_hex()
        ))
        .send()
        .await
        .expect("repeat drip request should not fail");

    assert_eq!(repeat.status(), StatusCode::TOO_MANY_REQUESTS);

    let repeat_body: serde_json::Value = repeat.json().await.expect("cooldown body is JSON");
    assert_eq!(repeat_body["status"], "cooldown");
    assert!(repeat_body["retry_after_secs"].as_u64().is_some());

    wait_for_balance(
        &validator.client,
        recipient_a.public_key(),
        1 + DRIP_AMOUNT,
        "recipient A",
    )
    .await;

    wait_for_balance(
        &validator.client,
        recipient_b.public_key(),
        2 + DRIP_AMOUNT,
        "recipient B",
    )
    .await;

    let faucet_balance = get_wallet_balance(&validator.client, faucet_account.public_key()).await;
    assert_eq!(
        faucet_balance,
        FAUCET_FUNDS - 2 * DRIP_AMOUNT,
        "faucet should have paid out exactly two drips"
    );
}

/// Runs the faucet worker and HTTP server in-process against the given node,
/// returning the faucet base URL.
async fn spawn_faucet(node: &NodeHttpClient, faucet_pk: ZkPublicKey) -> String {
    let faucet = Arc::new(
        Faucet::new(api_url(node, ""), faucet_pk, DRIP_AMOUNT).expect("faucet should be created"),
    );

    let (queue, requests) = mpsc::channel(DRIP_QUEUE_CAPACITY);
    tokio::spawn(run_worker(Arc::clone(&faucet), requests));

    let state = Arc::new(FaucetServerState::new(queue, COOLDOWN));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("faucet listener should bind");

    let address = listener.local_addr().expect("listener has address");
    tokio::spawn(async move {
        axum::serve(listener, faucet_app(state))
            .await
            .expect("faucet server should not fail");
    });

    format!("http://{address}")
}

async fn wait_for_balance(node: &NodeHttpClient, pk: ZkPublicKey, expected: u64, label: &str) {
    let deadline = tokio::time::Instant::now() + BALANCE_TIMEOUT;
    let mut last_balance = get_wallet_balance(node, pk).await;
    while tokio::time::Instant::now() < deadline {
        if last_balance == expected {
            return;
        }
        sleep(BALANCE_POLL_INTERVAL).await;
        last_balance = get_wallet_balance(node, pk).await;
    }
    panic!("timed out waiting for {label} balance {expected}, last balance was {last_balance}");
}
