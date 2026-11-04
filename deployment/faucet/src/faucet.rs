use std::{collections::HashMap, hash::Hash, sync::Arc, time::Duration};

use lb_common_http_client::CommonHttpClient;
use lb_http_api_common::bodies::wallet::{
    balance::WalletBalanceResponseBody,
    transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use reqwest::Url;
use tokio::sync::mpsc;

/// How often the worker polls the faucet balance while waiting for a
/// submitted drip to be included in a block.
const CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long the worker waits for a submitted drip to appear at the tip before
/// taking it as dropped. Generous compared to expected block times.
const CONFIRMATION_TIMEOUT: Duration = Duration::from_mins(2);
/// How many blocks below the tip a drip must be to count as confirmed, or at
/// the LIB if that is shallower. A drip at the tip can still be lost to a
/// fork, together with the change note the next drip would spend, while the
/// LIB of a real chain is far too deep to wait for on every drip.
const CONFIRMATION_DEPTH: usize = 6;
/// Pause between transfer attempts for the same drip request that has not been
/// submitted yet, so transient node errors are not retried in a tight loop.
const RETRY_DELAY: Duration = Duration::from_secs(5);
/// Transfer attempts per drip request before it is dropped. Bounds how long
/// one failing request can stall the queue: at most
/// `MAX_TRANSFER_ATTEMPTS * RETRY_DELAY` plus the request timeouts, or
/// `MAX_TRANSFER_ATTEMPTS * CONFIRMATION_TIMEOUT` once one attempt is
/// submitted.
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

    /// The notes of the faucet at the tip, and that tip.
    pub async fn wallet_at_tip(&self) -> Result<WalletBalanceResponseBody, String> {
        self.http_client
            .get_wallet_balance(self.base_url.clone(), self.faucet_pk, None)
            .await
            .map_err(|e| format!("Failed to fetch faucet balance: {e}"))
    }

    /// The faucet balance `CONFIRMATION_DEPTH` blocks below the tip, or at the
    /// LIB if that is shallower.
    async fn confirmed_balance(&self) -> Result<u64, String> {
        let info = self
            .http_client
            .consensus_info(self.base_url.clone())
            .await
            .map_err(|e| format!("Failed to fetch the chain tip: {e}"))?
            .cryptarchia_info;
        let mut block = info.tip;
        for _ in 0..CONFIRMATION_DEPTH {
            if block == info.lib {
                break;
            }
            block = self
                .http_client
                .get_block_by_id(self.base_url.clone(), block)
                .await
                .map_err(|e| format!("Failed to fetch block {block}: {e}"))?
                .ok_or_else(|| format!("Block {block} is not on the node"))?
                .header
                .parent_block;
        }
        self.http_client
            .get_wallet_balance(self.base_url.clone(), self.faucet_pk, Some(block))
            .await
            .map(|info| info.balance)
            .map_err(|e| format!("Failed to fetch faucet balance at {block}: {e}"))
    }

    /// Transfers to `recipient_pk` from the notes of `wallet`, at its tip.
    pub async fn transfer_to_pk(
        &self,
        recipient_pk: ZkPublicKey,
        wallet: &WalletBalanceResponseBody,
    ) -> Result<WalletTransferFundsResponseBody, String> {
        let current_balance = wallet.balance;

        let amount_to_send = std::cmp::min(current_balance, self.drip_amount);
        if amount_to_send == 0 {
            return Err(format!(
                "Balance too low to drip (Current: {current_balance}, ask for direct transfter in discord)"
            ));
        }

        println!("Dripping {amount_to_send} units to {recipient_pk:?}");

        let body = WalletTransferFundsRequestBody {
            tip: Some(wallet.tip),
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

    /// Waits until the faucet balance is below `balance_before` at the
    /// confirmation depth, which signals that the submitted drip is unlikely to
    /// be lost to a fork and the change note is spendable again. Returns false
    /// if the drip has not been at the tip for `CONFIRMATION_TIMEOUT`.
    async fn wait_for_confirmation(&self, balance_before: u64) -> bool {
        let mut deadline = tokio::time::Instant::now() + CONFIRMATION_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if let Ok(wallet) = self.wallet_at_tip().await
                && wallet.balance < balance_before
            {
                if let Ok(balance) = self.confirmed_balance().await
                    && balance < balance_before
                {
                    return true;
                }
                deadline = tokio::time::Instant::now() + CONFIRMATION_TIMEOUT;
            }
            tokio::time::sleep(CONFIRMATION_POLL_INTERVAL).await;
        }
        eprintln!("Timed out waiting for drip confirmation");
        false
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

/// Submits the drip until it is confirmed. A submitted drip that does not show
/// up may have been dropped from the mempools, for example when the note it
/// spends was lost to a fork, so it is submitted again, as long as that cannot
/// pay the recipient twice.
async fn drip_with_retries(faucet: &Faucet, recipient_pk: ZkPublicKey) {
    let mut first_attempt = None;
    for attempt in 1..=MAX_TRANSFER_ATTEMPTS {
        match submit_drip(faucet, recipient_pk, first_attempt.as_ref()).await {
            Ok(wallet) => {
                first_attempt.get_or_insert(wallet);
            }
            Err(e) => {
                eprintln!(
                    "Drip attempt {attempt}/{MAX_TRANSFER_ATTEMPTS} failed for {recipient_pk:?}: {e}"
                );
            }
        }
        // A failed attempt may be a note still reserved for the first one, whose
        // transaction can land in the meantime.
        match &first_attempt {
            Some(first) => {
                if faucet.wait_for_confirmation(first.balance).await {
                    return;
                }
            }
            None => tokio::time::sleep(RETRY_DELAY).await,
        }
    }
    eprintln!("Dropping drip request for {recipient_pk:?} after {MAX_TRANSFER_ATTEMPTS} attempts");
}

/// Submits the drip from the notes at the tip, and returns them. The transfer
/// is funded at that tip, so that the notes checked are the ones it spends.
async fn submit_drip(
    faucet: &Faucet,
    recipient_pk: ZkPublicKey,
    first_attempt: Option<&WalletBalanceResponseBody>,
) -> Result<WalletBalanceResponseBody, String> {
    let wallet = faucet.wallet_at_tip().await?;
    if let Some(first) = first_attempt
        && !conflicts_with_first_attempt(&first.notes, &wallet.notes)
    {
        return Err("the first attempt can still land next to a new one".to_owned());
    }
    let response = faucet.transfer_to_pk(recipient_pk, &wallet).await?;
    println!(
        "Drip submitted for {recipient_pk:?}, hash: {:?}, note: {:?}",
        response.hash, response.recipient_note
    );
    Ok(wallet)
}

/// Whether a transfer funded from `held_now` cannot land next to the first
/// attempt, funded from `funded_first_attempt`: both then spend the same note,
/// the only one the faucet had and still has. With several notes, the wallet
/// may fund each attempt from another one, and with the note gone the first
/// attempt has landed and its change would fund a second payment.
fn conflicts_with_first_attempt<Note: Eq + Hash>(
    funded_first_attempt: &HashMap<Note, u64>,
    held_now: &HashMap<Note, u64>,
) -> bool {
    funded_first_attempt.len() == 1 && funded_first_attempt == held_now
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_single_note_of_the_first_attempt_conflicts_with_it() {
        let single = HashMap::from([("n", 100)]);
        assert!(conflicts_with_first_attempt(&single, &single));
        // the first attempt landed: this note is its change, a second payment
        let change = HashMap::from([("c", 49)]);
        assert!(!conflicts_with_first_attempt(&single, &change));
        // another note can fund a transfer that spends nothing of the first attempt
        let topped_up = HashMap::from([("n", 100), ("t", 500)]);
        assert!(!conflicts_with_first_attempt(&single, &topped_up));
        // the first attempt may have spent the note that is not funding this one
        assert!(!conflicts_with_first_attempt(&topped_up, &topped_up));
    }
}
