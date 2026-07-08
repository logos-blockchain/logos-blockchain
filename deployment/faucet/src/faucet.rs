use std::{sync::Arc, time::Duration};

use lb_common_http_client::CommonHttpClient;
use lb_http_api_common::bodies::wallet::transfer_funds::{
    WalletTransferFundsRequestBody, WalletTransferFundsResponseBody,
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use reqwest::Url;
use tokio::sync::mpsc;

/// How often the worker polls the faucet balance while waiting for a
/// submitted drip to be included in a block.
const CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long the worker waits for a submitted drip to be confirmed before
/// moving on. Generous compared to expected block times; hitting it means the
/// transaction was likely dropped, and the next drip resyncs from the current
/// wallet state either way.
const CONFIRMATION_TIMEOUT: Duration = Duration::from_mins(2);
/// Pause between transfer attempts for the same drip request, so transient
/// node errors are not retried in a tight loop.
const RETRY_DELAY: Duration = Duration::from_secs(5);
/// Transfer attempts per drip request before it is dropped. Bounds how long
/// one failing request can stall the queue: at most
/// `MAX_TRANSFER_ATTEMPTS * RETRY_DELAY` plus the request timeouts.
const MAX_TRANSFER_ATTEMPTS: u32 = 5;

pub struct Faucet {
    faucet_pk: ZkPublicKey,
    drip_amount: u64,
    http_client: CommonHttpClient,
    base_url: Url,
}

impl Faucet {
    pub fn new(base_url: Url, faucet_pk: ZkPublicKey, drip_amount: u64) -> Result<Self, String> {
        let http_client = CommonHttpClient::new(None);

        Ok(Self {
            faucet_pk,
            drip_amount,
            http_client,
            base_url,
        })
    }

    pub async fn balance(&self) -> Result<u64, String> {
        self.http_client
            .get_wallet_balance(self.base_url.clone(), self.faucet_pk, None)
            .await
            .map(|info| info.balance)
            .map_err(|e| format!("Failed to fetch faucet balance: {e}"))
    }

    pub async fn transfer_to_pk(
        &self,
        recipient_pk: ZkPublicKey,
    ) -> Result<WalletTransferFundsResponseBody, String> {
        let current_balance = self.balance().await?;

        let amount_to_send = std::cmp::min(current_balance, self.drip_amount);
        if amount_to_send == 0 {
            return Err(format!(
                "Balance too low to drip (Current: {current_balance}, ask for direct transfter in discord)"
            ));
        }

        println!("Dripping {amount_to_send} units to {recipient_pk:?}");

        let body = WalletTransferFundsRequestBody {
            tip: None,
            change_public_key: self.faucet_pk,
            funding_public_keys: vec![self.faucet_pk],
            recipient_public_key: recipient_pk,
            amount: amount_to_send,
        };

        self.http_client
            .transfer_funds(self.base_url.clone(), body)
            .await
            .map_err(|e| format!("Faucet transfer failed: {e}"))
    }

    /// Waits until the faucet balance differs from `balance_before`, which
    /// signals that the previously submitted drip was included in a block and
    /// the change note is spendable again.
    async fn wait_for_confirmation(&self, balance_before: u64) {
        let deadline = tokio::time::Instant::now() + CONFIRMATION_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if let Ok(balance) = self.balance().await
                && balance != balance_before
            {
                return;
            }
            tokio::time::sleep(CONFIRMATION_POLL_INTERVAL).await;
        }
        eprintln!("Timed out waiting for drip confirmation; continuing with next request");
    }
}

/// Drains queued drip requests one at a time.
///
/// Each drip waits for on-chain confirmation before the next starts. The
/// faucet wallet holds a single note, so concurrent transfers would conflict
/// on the same input; serializing on confirmation guarantees each drip funds
/// from the confirmed change of the previous one.
pub async fn run_worker(faucet: Arc<Faucet>, mut requests: mpsc::Receiver<ZkPublicKey>) {
    while let Some(recipient_pk) = requests.recv().await {
        drip_with_retries(&faucet, recipient_pk).await;
    }
}

async fn drip_with_retries(faucet: &Faucet, recipient_pk: ZkPublicKey) {
    for attempt in 1..=MAX_TRANSFER_ATTEMPTS {
        let balance_before = match faucet.balance().await {
            Ok(balance) => balance,
            Err(e) => {
                eprintln!("Drip attempt {attempt}/{MAX_TRANSFER_ATTEMPTS}: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        match faucet.transfer_to_pk(recipient_pk).await {
            Ok(response) => {
                println!(
                    "Drip submitted for {recipient_pk:?}, hash: {:?}",
                    response.hash
                );
                faucet.wait_for_confirmation(balance_before).await;
                return;
            }
            Err(e) => {
                eprintln!(
                    "Drip attempt {attempt}/{MAX_TRANSFER_ATTEMPTS} failed for {recipient_pk:?}: {e}"
                );
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
    eprintln!("Dropping drip request for {recipient_pk:?} after {MAX_TRANSFER_ATTEMPTS} attempts");
}
