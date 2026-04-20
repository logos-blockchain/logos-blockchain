use std::{num::NonZero, time::Duration};

use lb_http_api_common::paths::CRYPTARCHIA_INFO;
use reqwest::Client;
use tokio::{task::JoinHandle, time::sleep};
use tracing::{info, warn};

use crate::cucumber::{steps::TARGET, utils::truncate_hash};

const FAUCET_BACKEND: &str = "/web/faucet-backend";

/// Background task that periodically checks the block height and requests funds
/// from the faucet.
pub struct FaucetTask {
    username: String,
    password: String,
    wallet_addresses: Vec<String>,
    last_height: u64,
    rounds_per_wallet: NonZero<usize>,
    faucet_url: String,
    check_height_urls: Vec<String>,
}

impl FaucetTask {
    /// Creates a new `FaucetTask` with the given parameters.
    pub fn new(
        base_url: &str,
        username: &str,
        password: &str,
        wallet_addresses: &[String],
        rounds_per_wallet: NonZero<usize>,
    ) -> Self {
        Self {
            username: username.to_owned(),
            password: password.to_owned(),
            wallet_addresses: wallet_addresses.to_owned(),
            last_height: 0,
            rounds_per_wallet,
            faucet_url: format!("{base_url}{FAUCET_BACKEND}"),
            check_height_urls: (0..=3)
                .map(|i| format!("{base_url}/node/{i}{CRYPTARCHIA_INFO}"))
                .collect(),
        }
    }

    /// Spawns the faucet task as a background async task that runs until it has
    /// completed the specified number of rounds per wallet. One request for one
    /// wallet address is made per block height increase (round-robin). The task
    /// periodically checks the block height every `poll_interval_ms`
    /// milliseconds and requests funds from the faucet when a new block is
    /// detected. This is a best effort task and funding is not guaranteed.
    pub fn spawn(self, poll_interval_ms: u64, step: &str) -> JoinHandle<()> {
        let Self {
            username,
            password,
            wallet_addresses,
            mut last_height,
            rounds_per_wallet,
            faucet_url,
            check_height_urls,
        } = self;
        if wallet_addresses.is_empty() {
            warn!(
                target: TARGET,
                "Step `{step}` no wallet addresses provided, skipping faucet task."
            );
            return tokio::spawn(async move {});
        }

        let client = Client::new();
        let step = step.to_owned();

        tokio::spawn(async move {
            info!(target: TARGET, "Faucet request(s) start");
            let mut next_index: usize = 0;
            let mut number_of_loops = 0;
            loop {
                match Self::check_block_height(&client, &check_height_urls, &username, &password)
                    .await
                {
                    Ok(height) => {
                        if height > last_height {
                            let address = &wallet_addresses[next_index];
                            info!(
                                target: TARGET,
                                "Faucet request {number_of_loops}/{} for `{} ...` at height \
                                `{height}` from `{}`",
                                 wallet_addresses.len() * rounds_per_wallet.get(),
                                truncate_hash(address, 16),
                                &check_height_urls[0],
                            );
                            last_height = height;

                            // Process only one wallet address per height increase (round-robin).
                            let post_url = format!("{faucet_url}/{address}");
                            if let Err(e) = Self::request_funds(&client, post_url).await {
                                warn!(
                                    target: TARGET,
                                    "Step `{step}` [request_funds] error for address `{address}`: {e}"
                                );
                            }

                            next_index = (next_index + 1) % wallet_addresses.len();
                            number_of_loops += 1;
                            if number_of_loops >= wallet_addresses.len() * rounds_per_wallet.get() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            target: TARGET,
                            "Step `{step}` [check_block_height] error: {e}"
                        );
                    }
                }

                sleep(Duration::from_millis(poll_interval_ms)).await;
            }
            info!(target: TARGET, "Faucet request(s) ended");
        })
    }

    async fn check_block_height(
        client: &Client,
        check_height_urls: &[String],
        username: &str,
        password: &str,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let mut last_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;

        for url in check_height_urls {
            match client
                .get(url)
                .basic_auth(username, Some(password))
                .send()
                .await
            {
                Ok(response) => {
                    if !response.status().is_success() {
                        last_err = Some(
                            format!("[check_block_height] HTTP {} from {url}", response.status())
                                .into(),
                        );
                        continue;
                    }

                    match response.json::<serde_json::Value>().await {
                        Ok(data) => {
                            if let Some(height) =
                                data.get("height").and_then(serde_json::Value::as_u64)
                            {
                                return Ok(height);
                            }
                            last_err = Some(
                                format!(
                                    "[check_block_height] Missing or invalid `height` in JSON \
                                        from {url}",
                                )
                                .into(),
                            );
                        }
                        Err(e) => {
                            last_err = Some(Box::new(e));
                        }
                    }
                }
                Err(e) => {
                    last_err = Some(Box::new(e));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            Box::new(std::io::Error::other(
                "[check_block_height] No response from any node",
            ))
        }))
    }

    async fn request_funds(
        client: &Client,
        post_url: String,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let resp = client
            .post(post_url)
            .send()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

        if !status.is_success() {
            return Err(format!("[request_funds] Request failed: {status} - {body}").into());
        }

        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

        json.get("hash").and_then(|v| v.as_str()).map_or_else(
            || Err(format!("[request_funds] Missing or invalid `hash` in response: {body}").into()),
            |hash| Ok(hash.to_owned()),
        )
    }
}
