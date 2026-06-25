use std::{collections::HashMap, num::NonZero, time::Duration};

use reqwest::Client;
use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::{info, warn};

use crate::cucumber::{
    error::StepError,
    steps::{TARGET, manual_nodes::utils::fetch_public_peer_consensus},
    utils::truncate_hash,
    world::PublicCryptarchiaEndpointPeer,
};

/// Background task that periodically checks the block height and requests funds
/// from the faucet.
pub struct FaucetTask {
    wallet_addresses: Vec<String>,
    last_height: u64,
    rounds_per_wallet: NonZero<usize>,
    faucet_url: String,
    cryptarchia_endpoint_peers: Vec<PublicCryptarchiaEndpointPeer>,
}

impl FaucetTask {
    /// Creates a new `FaucetTask` with the given parameters.
    pub fn new(
        faucet_url: &str,
        wallet_addresses: &[String],
        rounds_per_wallet: NonZero<usize>,
        cryptarchia_endpoint_peers: Vec<PublicCryptarchiaEndpointPeer>,
    ) -> Self {
        Self {
            wallet_addresses: wallet_addresses.to_owned(),
            last_height: 0,
            rounds_per_wallet,
            faucet_url: faucet_url.to_owned(),
            cryptarchia_endpoint_peers,
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
            wallet_addresses,
            mut last_height,
            rounds_per_wallet,
            faucet_url,
            cryptarchia_endpoint_peers,
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
            let total_requests = wallet_addresses.len() * rounds_per_wallet.get();
            loop {
                match Self::check_block_height(&client, &cryptarchia_endpoint_peers).await {
                    Ok(height) => {
                        if height > last_height {
                            number_of_loops += 1;
                            let address = &wallet_addresses[next_index];
                            last_height = height;

                            // Process only one wallet address per height increase (round-robin).
                            let post_url = format!("{faucet_url}/{address}");
                            match Self::request_funds(&client, post_url).await {
                                Ok(tx_hash) => {
                                    info!(
                                        target: TARGET,
                                        "Faucet request {number_of_loops}/{total_requests} for `{} ...` at height \
                                        `{height}` from `{faucet_url}`, hash: `{} ...`",
                                        truncate_hash(address, 16),
                                        truncate_hash(&tx_hash, 16),
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        target: TARGET,
                                        "Step `{step}` [request_funds] error for address `{address}`: {e}"
                                    );
                                }
                            }

                            next_index = (next_index + 1) % wallet_addresses.len();
                            if number_of_loops >= total_requests {
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
        cryptarchia_endpoint_peers: &[PublicCryptarchiaEndpointPeer],
    ) -> Result<u64, StepError> {
        if cryptarchia_endpoint_peers.is_empty() {
            return Err(StepError::LogicalError {
                message: "[check_block_height] No peers configured".to_owned(),
            });
        }

        // Launch all peer requests concurrently with per-request timeout.
        let futs = cryptarchia_endpoint_peers.iter().map(async |peer| {
            match timeout(
                Duration::from_secs(2),
                fetch_public_peer_consensus(client, peer),
            )
            .await
            {
                Ok(Ok(info)) => Ok(info.height),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(StepError::LogicalError {
                    message: format!("[check_block_height] Timeout from peer `{}`", peer.base_url),
                }),
            }
        });

        let results = futures::future::join_all(futs).await;

        let mut counts: HashMap<u64, usize> = HashMap::new();
        let mut last_err: Option<StepError> = None;

        for r in results {
            match r {
                Ok(height) => {
                    *counts.entry(height).or_insert(0) += 1;
                }
                Err(e) => last_err = Some(e),
            }
        }

        if counts.is_empty() {
            return Err(last_err.unwrap_or_else(|| StepError::LogicalError {
                message: "[check_block_height] No successful response from any node".to_owned(),
            }));
        }

        // Pick the most frequent height (majority/plurality).
        // If tied, choose the higher height as deterministic tie-breaker.
        let best = counts
            .into_iter()
            .max_by(|(h1, c1), (h2, c2)| c1.cmp(c2).then_with(|| h1.cmp(h2)))
            .map(|(h, _)| h);

        best.ok_or_else(|| StepError::LogicalError {
            message: "[check_block_height] Unable to determine majority height".to_owned(),
        })
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
