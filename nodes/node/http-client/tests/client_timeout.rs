use std::time::{Duration, Instant};

use logos_blockchain_common_http_client::{CommonHttpClient, Error};
use reqwest::Url;

#[test]
fn a_supplied_client_decides_how_long_a_request_may_wait() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        // A server that accepts the connection and never answers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                // Keep the connection open and never answer on it.
                tokio::spawn(async move {
                    let _held = stream;
                    futures::future::pending::<()>().await;
                });
            }
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(300))
            .build()
            .unwrap();
        let client = CommonHttpClient::with_client(client, None);
        let url = Url::parse(&format!("http://{address}/")).unwrap();

        let started = Instant::now();
        let result = client.get::<(), serde_json::Value>(url, None).await;

        match result {
            Err(Error::Request(error)) => assert!(error.is_timeout(), "{error}"),
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(5));
    });
}
