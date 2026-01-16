use std::{collections::HashSet, fs, io, path::Path, time::Duration};

use common_http_client::CommonHttpClient;
use demo_sequencer::{
    BlockData, Transaction, TransferRequest, TransferResponse,
    db::{AccountDb, DbError},
};
use key_management_system_service::keys::{ED25519_SECRET_KEY_SIZE, Ed25519Key};
use nomos_core::{
    header::HeaderId,
    mantle::{
        MantleTx, SignedMantleTx, Transaction as _,
        ledger::Tx as LedgerTx,
        ops::{
            Op, OpProof,
            channel::{ChannelId, Ed25519PublicKey, MsgId, inscribe::InscriptionOp},
        },
        tx::TxHash,
    },
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::sleep;
use tracing::{debug, info, warn};

#[derive(Debug, Error)]
pub enum SequencerError {
    #[error("Database error: {0}")]
    Db(#[from] Box<DbError>),
    #[error("HTTP client error: {0}")]
    Http(#[from] common_http_client::Error),
    #[error("URL parse error: {0}")]
    Url(String),
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Invalid key file: expected {expected} bytes, got {actual}")]
    InvalidKeyFile { expected: usize, actual: usize },
    #[error("{0}")]
    InvalidChannelId(String),
    #[error("Transaction not included after timeout")]
    Timeout,
    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<DbError> for SequencerError {
    fn from(err: DbError) -> Self {
        Self::Db(Box::new(err))
    }
}

pub type Result<T> = std::result::Result<T, SequencerError>;

/// Pending transfer stored in the DB queue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTransfer {
    pub tx_id: String,
    #[serde(default)]
    pub tx_index: u64,
    pub request: TransferRequest,
    pub from_balance: u64,
    pub to_balance: u64,
}

/// The sequencer that handles transactions
pub struct Sequencer {
    db: AccountDb,
    http_client: CommonHttpClient,
    node_url: Url,
    signing_key: Ed25519Key,
    channel_id: ChannelId,
}

const MAX_DEPTH_PER_POLL: usize = 50;

fn empty_ledger_signature(tx_hash: &TxHash) -> key_management_system_service::keys::ZkSignature {
    key_management_system_service::keys::ZkKey::multi_sign(&[], tx_hash.as_ref())
        .expect("multi-sign with empty key set works")
}

/// Load signing key from file or generate a new one if it doesn't exist
fn load_or_create_signing_key(path: &Path) -> Result<Ed25519Key> {
    if path.exists() {
        debug!("Loading existing signing key from {:?}", path);
        let key_bytes = fs::read(path)?;
        if key_bytes.len() != ED25519_SECRET_KEY_SIZE {
            return Err(SequencerError::InvalidKeyFile {
                expected: ED25519_SECRET_KEY_SIZE,
                actual: key_bytes.len(),
            });
        }
        let key_array: [u8; ED25519_SECRET_KEY_SIZE] =
            key_bytes.try_into().expect("length already checked");
        Ok(Ed25519Key::from_bytes(&key_array))
    } else {
        debug!("Generating new signing key and saving to {:?}", path);
        let mut key_bytes = [0u8; ED25519_SECRET_KEY_SIZE];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key_bytes);
        fs::write(path, key_bytes)?;
        Ok(Ed25519Key::from_bytes(&key_bytes))
    }
}

impl Sequencer {
    pub fn new(
        db: AccountDb,
        node_endpoint: &str,
        signing_key_path: &str,
        channel_id_str: &str,
        node_auth_username: Option<String>,
        node_auth_password: Option<String>,
    ) -> Result<Self> {
        let node_url = Url::parse(node_endpoint).map_err(|e| SequencerError::Url(e.to_string()))?;

        let basic_auth = node_auth_username.map(|username| {
            common_http_client::BasicAuthCredentials::new(username, node_auth_password)
        });
        let http_client = CommonHttpClient::new(basic_auth);

        // Load or generate the signing key
        let signing_key = load_or_create_signing_key(Path::new(signing_key_path))?;

        // Decode channel ID from 64-character hex string (32 bytes)
        let decoded = hex::decode(channel_id_str).map_err(|_| {
            SequencerError::InvalidChannelId(format!(
                "SEQUENCER_CHANNEL_ID must be a valid hex string, got: '{channel_id_str}'"
            ))
        })?;
        let channel_bytes: [u8; 32] = decoded.try_into().map_err(|v: Vec<u8>| {
            SequencerError::InvalidChannelId(format!(
                "SEQUENCER_CHANNEL_ID must be exactly 64 hex characters (32 bytes), got {} characters ({} bytes)",
                v.len() * 2,
                v.len()
            ))
        })?;
        let channel_id = ChannelId::from(channel_bytes);
        info!("Channel ID: {}", hex::encode(channel_id.as_ref()));

        Ok(Self {
            db,
            http_client,
            node_url,
            signing_key,
            channel_id,
        })
    }

    /// Get the last message ID from the database, or root if not set
    async fn get_last_msg_id(&self) -> Result<MsgId> {
        (self.db.get_last_msg_id().await?)
            .map_or_else(|| Ok(MsgId::root()), |bytes| Ok(MsgId::from(bytes)))
    }

    /// Save the last message ID to the database
    async fn set_last_msg_id(&self, msg_id: MsgId) -> Result<()> {
        let bytes: [u8; 32] = msg_id.into();
        self.db.set_last_msg_id(&bytes).await?;
        Ok(())
    }

    /// Create and sign a transaction for inscribing data
    fn create_inscribe_tx(&self, data: Vec<u8>, parent: MsgId) -> SignedMantleTx {
        let verifying_key_bytes = self.signing_key.public_key().to_bytes();
        let verifying_key =
            Ed25519PublicKey::from_bytes(&verifying_key_bytes).expect("valid ed25519 public key");

        let inscribe_op = InscriptionOp {
            channel_id: self.channel_id,
            inscription: data,
            parent,
            signer: verifying_key,
        };

        let ledger_tx = LedgerTx::new(vec![], vec![]);

        let inscribe_tx = MantleTx {
            ops: vec![Op::ChannelInscribe(inscribe_op)],
            ledger_tx,
            storage_gas_price: 0,
            execution_gas_price: 0,
        };

        let tx_hash = inscribe_tx.hash();
        let signature_bytes = self
            .signing_key
            .sign_payload(tx_hash.as_signing_bytes().as_ref())
            .to_bytes();
        let signature =
            key_management_system_service::keys::Ed25519Signature::from_bytes(&signature_bytes);

        SignedMantleTx {
            ops_proofs: vec![OpProof::Ed25519Sig(signature)],
            ledger_tx_proof: empty_ledger_signature(&tx_hash),
            mantle_tx: inscribe_tx,
        }
    }

    /// Post a transaction to the node and wait for inclusion
    async fn post_and_wait(&self, tx: &SignedMantleTx) -> Result<()> {
        // Post the transaction
        self.http_client
            .post_transaction(self.node_url.clone(), tx.clone())
            .await?;

        debug!("Transaction posted, waiting for inclusion...");

        // Wait for the transaction to be included
        self.wait_for_inclusion(tx).await?;

        Ok(())
    }

    fn block_contains_inscription(
        block: &nomos_core::block::Block<SignedMantleTx>,
        expected: &InscriptionOp,
        block_id: HeaderId,
    ) -> bool {
        for tx in block.transactions() {
            for op in &tx.mantle_tx.ops {
                if let Op::ChannelInscribe(inscribe) = op {
                    tracing::debug!(
                        "Found inscription: channel={}, parent={}",
                        hex::encode(inscribe.channel_id.as_ref()),
                        hex::encode(<[u8; 32]>::from(inscribe.parent))
                    );

                    if inscribe.inscription == expected.inscription
                        && inscribe.channel_id == expected.channel_id
                        && inscribe.parent == expected.parent
                    {
                        debug!("Transaction included in block {}", block_id);
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Walk back from tip checking blocks for the expected inscription
    async fn check_blocks_for_inscription(
        &self,
        expected: &InscriptionOp,
        checked_blocks: &mut HashSet<HeaderId>,
        tip: HeaderId,
    ) -> Result<bool> {
        let mut current_id = Some(tip);
        let mut depth = 0;

        while let Some(block_id) = current_id {
            if checked_blocks.contains(&block_id) || depth >= MAX_DEPTH_PER_POLL {
                break;
            }

            let Some(block) = self
                .http_client
                .get_block(self.node_url.clone(), block_id)
                .await?
            else {
                break;
            };

            checked_blocks.insert(block_id);
            depth += 1;

            tracing::debug!(
                "Checking block {} (depth {}): {} transactions",
                block_id,
                depth,
                block.transactions().len()
            );

            if Self::block_contains_inscription(&block, expected, block_id) {
                return Ok(true);
            }

            current_id = Some(block.header().parent());
        }

        Ok(false)
    }

    fn get_expected_inscription(tx: &SignedMantleTx) -> &InscriptionOp {
        let expected_op = tx
            .mantle_tx
            .ops
            .first()
            .expect("transaction should have at least one op");

        let Op::ChannelInscribe(expected_inscription) = expected_op else {
            panic!("Expected ChannelInscribe op")
        };

        expected_inscription
    }

    async fn poll_for_inclusion(
        &self,
        expected: &InscriptionOp,
        checked_blocks: &mut HashSet<HeaderId>,
    ) -> Result<bool> {
        let info = self
            .http_client
            .consensus_info(self.node_url.clone())
            .await?;

        tracing::debug!(
            "Polling: tip={}, height={}, checked_blocks={}",
            info.tip,
            info.height,
            checked_blocks.len()
        );

        self.check_blocks_for_inscription(expected, checked_blocks, info.tip)
            .await
    }

    /// Wait for a transaction to be included in a block.
    async fn wait_for_inclusion(&self, tx: &SignedMantleTx) -> Result<()> {
        let expected_inscription = Self::get_expected_inscription(tx);

        let timeout_duration = Duration::from_mins(5);
        let poll_interval = Duration::from_millis(500);
        let start = std::time::Instant::now();
        let mut checked_blocks: HashSet<HeaderId> = HashSet::new();

        tracing::debug!(
            "Waiting for inscription: channel={}, parent={}",
            hex::encode(expected_inscription.channel_id.as_ref()),
            hex::encode(<[u8; 32]>::from(expected_inscription.parent))
        );

        while start.elapsed() < timeout_duration {
            if self
                .poll_for_inclusion(expected_inscription, &mut checked_blocks)
                .await?
            {
                return Ok(());
            }
            sleep(poll_interval).await;
        }

        warn!(
            "Timeout waiting for chain inclusion after {:?}",
            timeout_duration
        );
        Err(SequencerError::Timeout)
    }

    /// Process a transfer request - validates, updates DB, adds to queue,
    /// returns immediately
    pub async fn process_transfer(&self, request: TransferRequest) -> Result<TransferResponse> {
        info!(
            "TRANSFER {} -> {} ({} tokens)",
            request.from, request.to, request.amount
        );

        // Validate and update balances in the database first
        let (from_balance, to_balance) = self
            .db
            .transfer(&request.from, &request.to, request.amount)
            .await?;

        // Generate transaction ID
        let tx_id = {
            let mut id_bytes = [0u8; 16];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut id_bytes);
            hex::encode(id_bytes)
        };

        // Save transaction immediately with confirmed=false
        let tx_index = self.db.next_tx_index().await?;
        let tx = Transaction {
            id: tx_id.clone(),
            from: request.from.clone(),
            to: request.to.clone(),
            amount: request.amount,
            confirmed: false,
            index: tx_index,
        };
        let tx_data =
            serde_json::to_vec(&tx).map_err(|e| SequencerError::Serialization(e.to_string()))?;
        self.db.save_transaction(&tx_id, &tx_data).await?;

        // Create pending transfer and serialize for DB queue
        let pending = PendingTransfer {
            tx_id: tx_id.clone(),
            tx_index,
            request,
            from_balance,
            to_balance,
        };

        let data = serde_json::to_vec(&pending)
            .map_err(|e| SequencerError::Serialization(e.to_string()))?;

        // Add to DB queue
        self.db.queue_push(&tx_id, &data).await?;

        let queue_len = self.db.queue_len().await?;
        debug!("Queued tx {} (queue size: {})", tx_id, queue_len);

        // Return success immediately - actual on-chain posting happens in background
        Ok(TransferResponse {
            from_balance,
            to_balance,
            tx_hash: tx_id,
        })
    }

    /// Background processing loop - call this in a spawned task
    pub async fn run_processing_loop(&self) {
        let poll_interval = Duration::from_millis(100);

        loop {
            // Check if there are pending transfers
            let is_empty = match self.db.queue_is_empty().await {
                Ok(empty) => empty,
                Err(e) => {
                    tracing::error!("Failed to check queue: {}", e);
                    sleep(poll_interval).await;
                    continue;
                }
            };

            if is_empty {
                sleep(poll_interval).await;
                continue;
            }

            // Drain and process all pending transfers
            if let Err(e) = self.process_pending_batch().await {
                tracing::error!("Batch processing failed: {}", e);
            }
        }
    }

    fn deserialize_pending_transfers(items: &[(String, Vec<u8>)]) -> Vec<PendingTransfer> {
        let mut pending = Vec::new();
        for (tx_id, data) in items {
            match serde_json::from_slice::<PendingTransfer>(data) {
                Ok(p) => pending.push(p),
                Err(e) => {
                    tracing::error!("Failed to deserialize pending transfer {}: {}", tx_id, e);
                }
            }
        }
        pending
    }

    async fn revert_transfers(&self, pending: &[PendingTransfer]) {
        for p in pending {
            if let Err(revert_err) = self
                .db
                .transfer(&p.request.to, &p.request.from, p.request.amount)
                .await
            {
                tracing::error!(
                    "Failed to revert transfer {} -> {}: {}",
                    p.request.from,
                    p.request.to,
                    revert_err
                );
            } else {
                warn!(
                    "REVERTED {} -> {} ({} tokens)",
                    p.request.from, p.request.to, p.request.amount
                );
            }
        }
    }

    async fn confirm_transactions(&self, pending: &[PendingTransfer]) -> Result<()> {
        for p in pending {
            let tx = Transaction {
                id: p.tx_id.clone(),
                from: p.request.from.clone(),
                to: p.request.to.clone(),
                amount: p.request.amount,
                confirmed: true,
                index: p.tx_index,
            };
            let tx_data = serde_json::to_vec(&tx)
                .map_err(|e| SequencerError::Serialization(e.to_string()))?;
            self.db.save_transaction(&tx.id, &tx_data).await?;
        }
        Ok(())
    }

    async fn create_and_post_block(
        &self,
        pending: &[PendingTransfer],
    ) -> Result<(BlockData, MsgId)> {
        let (block_id, parent_block_id) = self.db.next_block_id().await?;
        let transactions: Vec<Transaction> = pending
            .iter()
            .map(|p| Transaction {
                id: p.tx_id.clone(),
                from: p.request.from.clone(),
                to: p.request.to.clone(),
                amount: p.request.amount,
                confirmed: false,
                index: p.tx_index,
            })
            .collect();

        let block_data = BlockData {
            block_id,
            parent_block_id,
            transactions,
        };

        let inscription_data = serde_json::to_vec(&block_data)
            .map_err(|e| SequencerError::Serialization(e.to_string()))?;

        info!(
            "BLOCK #{} (parent: #{}) posting to chain ({} tx)",
            block_id,
            parent_block_id,
            pending.len()
        );

        let parent = self.get_last_msg_id().await?;
        let tx = self.create_inscribe_tx(inscription_data, parent);

        let new_msg_id = match tx.mantle_tx.ops.first() {
            Some(Op::ChannelInscribe(inscribe)) => inscribe.id(),
            _ => panic!("Expected ChannelInscribe op"),
        };

        self.post_and_wait(&tx).await?;

        Ok((block_data, new_msg_id))
    }

    /// Process all pending transfers as a single block
    async fn process_pending_batch(&self) -> Result<()> {
        let items = self.db.queue_drain().await?;
        if items.is_empty() {
            return Ok(());
        }

        let pending = Self::deserialize_pending_transfers(&items);
        if pending.is_empty() {
            return Ok(());
        }

        let count = pending.len();
        debug!("Processing batch of {} transfers", count);

        match self.create_and_post_block(&pending).await {
            Ok((block_data, new_msg_id)) => {
                self.set_last_msg_id(new_msg_id).await?;
                self.confirm_transactions(&pending).await?;
                info!(
                    "BLOCK #{} confirmed on chain ({} tx)",
                    block_data.block_id, count
                );
                Ok(())
            }
            Err(e) => {
                self.revert_transfers(&pending).await;
                self.delete_transactions(&pending).await;
                Err(e)
            }
        }
    }

    async fn delete_transactions(&self, pending: &[PendingTransfer]) {
        for p in pending {
            if let Err(e) = self.db.delete_transaction(&p.tx_id).await {
                warn!("Failed to delete transaction {}: {}", p.tx_id, e);
            }
        }
    }

    /// Get the balance of an account
    pub async fn get_balance(&self, account: &str) -> Result<u64> {
        Ok(self.db.get_or_create_balance(account).await?)
    }

    /// List all accounts
    pub async fn list_accounts(&self) -> Result<Vec<(String, u64)>> {
        Ok(self.db.list_accounts().await?)
    }

    /// Get all transactions for an account (as sender or receiver), sorted by
    /// index
    pub async fn get_account_transactions(&self, account: &str) -> Result<Vec<Transaction>> {
        let all_txs = self.db.get_all_transactions_raw().await?;
        let mut transactions: Vec<Transaction> = all_txs
            .iter()
            .filter_map(|data| serde_json::from_slice::<Transaction>(data).ok())
            .filter(|tx| tx.from == account || tx.to == account)
            .collect();

        transactions.sort_by_key(|tx| tx.index);
        transactions.reverse();
        Ok(transactions)
    }

    /// Get confirmed balance based only on confirmed transactions
    pub async fn get_confirmed_balance(&self, account: &str) -> Result<u64> {
        let initial_balance = self.db.initial_balance();
        let all_txs = self.db.get_all_transactions_raw().await?;

        let mut balance = initial_balance;

        for data in all_txs {
            if let Ok(tx) = serde_json::from_slice::<Transaction>(&data)
                && tx.confirmed
            {
                if tx.from == account {
                    balance = balance.saturating_sub(tx.amount);
                }
                if tx.to == account {
                    balance = balance.saturating_add(tx.amount);
                }
            }
        }

        Ok(balance)
    }
}
