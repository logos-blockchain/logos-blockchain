use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::Arc,
    time::Duration,
};

use lb_common_http_client::{CommonHttpClient, Error as HttpError};
use lb_http_api_common::bodies::wallet::{
    balance::WalletBalanceResponseBody,
    transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_wallet::SPEND_ROOT_DEPTH;
use reqwest::Url;
use tokio::sync::mpsc;

/// How often the worker polls the faucet balance while waiting for a
/// submitted drip to be included in a block.
const CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long the worker waits for a submitted drip to appear at the tip before
/// taking it as dropped. Generous compared to expected block times.
const CONFIRMATION_TIMEOUT: Duration = Duration::from_mins(2);
/// How many blocks below the tip a drip must be to count as confirmed, or at
/// the LIB if that is shallower: the wallet spends the change note the next
/// drip needs from that depth on.
const CONFIRMATION_DEPTH: usize = SPEND_ROOT_DEPTH;
/// Pause between transfer attempts for the same drip request that has not been
/// submitted yet, so transient node errors are not retried in a tight loop.
const RETRY_DELAY: Duration = Duration::from_secs(5);
/// Transfer attempts per drip request before it is dropped. Until an attempt is
/// submitted, one failing request stalls the queue for at most
/// `MAX_TRANSFER_ATTEMPTS * RETRY_DELAY` plus the time the requests take, on
/// top of the wait for the notes that can fund it to become spendable. Once one
/// is, each attempt waits for confirmation until the balance has been back
/// at its value before the drip for `CONFIRMATION_TIMEOUT`, so a drip seen at
/// the tip holds the queue for as long as it stays there short of the
/// confirmation depth.
const MAX_TRANSFER_ATTEMPTS: u32 = 5;

/// A transfer that did not report success.
pub struct TransferFailure {
    reason: String,
    /// The node may have accepted the transaction all the same.
    may_be_submitted: bool,
}

/// Whether a transfer failing with `error` may still have reached the mempool:
/// the node's answer was lost or did not come from the handler, as with a
/// timeout. A refused connection never reached it, and an internal server
/// error is the handler refusing the transfer.
fn may_be_submitted(error: &HttpError) -> bool {
    match error {
        HttpError::Request(error) => !error.is_connect(),
        HttpError::UnexpectedResponse(_) => true,
        HttpError::Server(_) | HttpError::Client(_) | HttpError::Url(_) => false,
    }
}

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

    /// The notes of the faucet at the tip, once the drip they can pay is not
    /// held back by notes no spend can use yet. Those become spendable as
    /// the chain grows, so waiting uses up none of the transfer attempts.
    async fn wallet_past_pending_notes(&self) -> Result<WalletBalanceResponseBody, String> {
        let mut told = false;
        loop {
            let wallet = self.wallet_at_tip().await?;
            if !pending_notes_hold_the_drip(&wallet.notes, &wallet.pending_notes, self.drip_amount)
            {
                return Ok(wallet);
            }
            if !told {
                eprintln!("Waiting for pending notes to become spendable before the next drip");
                told = true;
            }
            tokio::time::sleep(CONFIRMATION_POLL_INTERVAL).await;
        }
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
    ) -> Result<WalletTransferFundsResponseBody, TransferFailure> {
        let current_balance = wallet.balance;

        let amount_to_send = std::cmp::min(current_balance, self.drip_amount);
        if amount_to_send == 0 {
            return Err(TransferFailure {
                reason: format!(
                    "Balance too low to drip (Current: {current_balance}, ask for direct transfter in discord)"
                ),
                may_be_submitted: false,
            });
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
            .map_err(|e| TransferFailure {
                may_be_submitted: may_be_submitted(&e),
                reason: format!("Faucet transfer failed: {e}"),
            })
    }

    /// Waits until the faucet balance differs from `balance_before` at the
    /// confirmation depth, which signals that the submitted drip is unlikely to
    /// be lost to a fork and the change note is spendable again. Returns false
    /// if the balance at the tip has been `balance_before` for
    /// `CONFIRMATION_TIMEOUT`.
    async fn wait_for_confirmation(&self, balance_before: u64) -> bool {
        let mut deadline = tokio::time::Instant::now() + CONFIRMATION_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if let Ok(wallet) = self.wallet_at_tip().await
                && wallet.balance != balance_before
            {
                if let Ok(balance) = self.confirmed_balance().await
                    && balance != balance_before
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
/// pay the recipient twice. A transfer whose answer was lost counts as
/// submitted, since it may land.
async fn drip_with_retries(faucet: &Faucet, recipient_pk: ZkPublicKey) {
    let mut first_attempt = None;
    for attempt in 1..=MAX_TRANSFER_ATTEMPTS {
        match submit_drip(faucet, recipient_pk, first_attempt.as_ref()).await {
            Ok(wallet) => {
                first_attempt.get_or_insert(wallet);
            }
            Err(failed) => {
                eprintln!(
                    "Drip attempt {attempt}/{MAX_TRANSFER_ATTEMPTS} failed for {recipient_pk:?}: {}",
                    failed.reason
                );
                if let Some(wallet) = failed.maybe_submitted_from {
                    first_attempt.get_or_insert(*wallet);
                }
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

/// An attempt at a drip that did not report success.
struct FailedAttempt {
    reason: String,
    /// The notes it was funded from, if its transaction may land all the same.
    maybe_submitted_from: Option<Box<WalletBalanceResponseBody>>,
}

impl From<String> for FailedAttempt {
    fn from(reason: String) -> Self {
        Self {
            reason,
            maybe_submitted_from: None,
        }
    }
}

/// Submits the drip from the notes at the tip, and returns them. The transfer
/// is funded at that tip, so that the notes checked are the ones it spends.
async fn submit_drip(
    faucet: &Faucet,
    recipient_pk: ZkPublicKey,
    first_attempt: Option<&WalletBalanceResponseBody>,
) -> Result<WalletBalanceResponseBody, FailedAttempt> {
    let wallet = match first_attempt {
        Some(_) => faucet.wallet_at_tip().await?,
        None => faucet.wallet_past_pending_notes().await?,
    };
    if let Some(first) = first_attempt
        && !conflicts_with_first_attempt(&first.notes, &wallet.notes)
    {
        return Err("the first attempt can still land next to a new one"
            .to_owned()
            .into());
    }
    let response = match faucet.transfer_to_pk(recipient_pk, &wallet).await {
        Ok(response) => response,
        Err(failure) => {
            return Err(FailedAttempt {
                reason: failure.reason,
                maybe_submitted_from: failure.may_be_submitted.then(|| Box::new(wallet)),
            });
        }
    };
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

/// Whether the spendable notes fall short of the drip `notes` can pay, so that
/// the rest of it sits in `pending`.
fn pending_notes_hold_the_drip<Note: Eq + Hash>(
    notes: &HashMap<Note, u64>,
    pending: &HashSet<Note>,
    drip_amount: u64,
) -> bool {
    let total: u64 = notes.values().sum();
    let spendable: u64 = notes
        .iter()
        .filter(|(note, _)| !pending.contains(*note))
        .map(|(_, value)| value)
        .sum();
    spendable < total.min(drip_amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_answer_from_the_handler_settles_a_transfer_as_not_submitted() {
        assert!(!may_be_submitted(&HttpError::Server("refused".to_owned())));
        // such as the 408 of the node's timeout, while the handler may have
        // added the transaction to the mempool
        assert!(may_be_submitted(&HttpError::UnexpectedResponse(
            "[408]".to_owned()
        )));
    }

    #[test]
    fn a_drip_waits_only_for_pending_notes_that_hold_it() {
        let notes = HashMap::from([("spendable", 40), ("young", 100)]);
        let pending = HashSet::from(["young"]);
        assert!(pending_notes_hold_the_drip(&notes, &pending, 50));
        // what is spendable pays the drip, whatever else is pending
        assert!(!pending_notes_hold_the_drip(&notes, &pending, 40));
        // nothing is pending: waiting would change nothing, however short the funds
        assert!(!pending_notes_hold_the_drip(&notes, &HashSet::new(), 500));
    }

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
