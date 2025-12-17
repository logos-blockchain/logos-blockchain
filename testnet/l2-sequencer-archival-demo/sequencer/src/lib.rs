use serde::{Deserialize, Serialize};

/// Request to transfer funds between accounts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRequest {
    pub from: String,
    pub to: String,
    pub amount: u64,
}

/// Transaction with unique ID for on-chain inscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub from: String,
    pub to: String,
    pub amount: u64,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub index: u64,
}

/// Response after successful transfer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferResponse {
    pub from_balance: u64,
    pub to_balance: u64,
    pub tx_hash: String,
}

/// Block data format inscribed on-chain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockData {
    pub block_id: u64,
    pub transactions: Vec<Transaction>,
}
