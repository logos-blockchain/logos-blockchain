use lb_core::mantle::{Value, ops::channel::ChannelKeyIndex};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
/// Wallet funds exported by the cucumber manual `EXPORT_FUNDS` command.
pub struct WalletFundsExport {
    /// File format version.
    pub version: u8,
    /// File kind discriminator.
    pub kind: String,
    /// Name of the cucumber wallet that exported the funds.
    pub wallet: String,
    /// HTTP endpoint of the node associated with the exporting wallet.
    #[serde(alias = "node")]
    pub node_url: String,
    /// Hex-encoded recipient wallet public key.
    pub public_key: String,
    /// Optional hex-encoded wallet secret key for spending exported funds.
    pub secret_key: Option<String>,
    /// Amount requested by the export command.
    pub requested_value: u64,
    /// Total value selected by the export command.
    pub selected_value: u64,
    /// Exported spendable UTXOs.
    pub utxos: Vec<ExportedUtxo>,
}

#[derive(Serialize, Deserialize)]
/// One UTXO entry in a wallet funds export.
pub struct ExportedUtxo {
    /// Hex-encoded UTXO identifier.
    pub utxo_id: String,
    /// Value carried by the UTXO.
    pub value: u64,
    /// Hex-encoded bincode representation of the UTXO.
    pub encoded_utxo: String,
}

#[derive(Serialize, Deserialize)]
/// Summary written after building a deposit transaction.
pub struct DepositSubmission {
    /// File format version.
    pub version: u8,
    /// File kind discriminator.
    pub kind: String,
    /// Hex-encoded zone channel identifier.
    pub channel_id: String,
    /// Deposited value.
    pub amount: Value,
    /// Hex-encoded signed transaction hash.
    pub tx_hash: String,
    /// Hex-encoded inscription message identifier.
    pub msg_id: String,
    /// Hex-encoded public key that funded the deposit.
    pub recipient_wallet_public_key: String,
    /// Hex-encoded input IDs reserved by the deposit transaction.
    pub reserved_input_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
/// Unsigned withdrawal transaction plus metadata required for offline signing.
pub struct WithdrawIntent {
    /// File format version.
    pub version: u8,
    /// File kind discriminator.
    pub kind: String,
    /// Hex-encoded zone channel identifier.
    pub channel_id: String,
    /// Hex-encoded transaction hash that signers must sign.
    pub tx_hash: String,
    /// Hex-encoded inscription message identifier.
    pub msg_id: String,
    /// Number of unique authorized signatures required.
    pub required_threshold: u16,
    /// Hex-encoded mantle transaction bytes.
    pub mantle_tx: String,
    /// Hex-encoded sequencer signature for the inscription op.
    pub inscription_signature: String,
    /// Withdrawal output metadata included for operators.
    pub withdraws: Vec<WithdrawFileEntry>,
    /// Authorized channel signers indexed by channel key index.
    pub authorized_signers: Vec<AuthorizedSigner>,
    /// Signatures already attached to the intent.
    pub signatures: Vec<WithdrawSignatureEntry>,
}

#[derive(Serialize, Deserialize)]
/// Human-readable withdrawal output metadata.
pub struct WithdrawFileEntry {
    /// Withdrawn value.
    pub amount: Value,
    /// Hex-encoded recipient public key.
    pub recipient_public_key: String,
    /// Channel withdrawal nonce used by the transaction.
    pub withdraw_nonce: u32,
}

#[derive(Serialize, Deserialize, Clone)]
/// Authorized channel signer metadata.
pub struct AuthorizedSigner {
    /// Index of the key in the channel's accredited key list.
    pub key_index: ChannelKeyIndex,
    /// Hex-encoded Ed25519 public key.
    pub public_key: String,
}

#[derive(Serialize, Deserialize, Clone)]
/// Signature entry embedded in a withdrawal intent or signed withdrawal file.
pub struct WithdrawSignatureEntry {
    /// Index of the signer in the channel's accredited key list.
    pub signer_key_index: ChannelKeyIndex,
    /// Hex-encoded Ed25519 public key for the signer.
    pub signer_public_key: String,
    /// Hex-encoded bincode Ed25519 signature over the transaction hash.
    pub signature: String,
}

#[derive(Serialize, Deserialize)]
/// Standalone signature file produced by `withdraw sign`.
pub struct WithdrawSignatureFile {
    /// File format version.
    pub version: u8,
    /// File kind discriminator.
    pub kind: String,
    /// Hex-encoded zone channel identifier.
    pub channel_id: String,
    /// Hex-encoded transaction hash this signature covers.
    pub tx_hash: String,
    /// Hex-encoded Ed25519 public key for the signer.
    pub signer_public_key: String,
    /// Index of the signer in the channel's accredited key list.
    pub signer_key_index: ChannelKeyIndex,
    /// Hex-encoded bincode Ed25519 signature over the transaction hash.
    pub signature: String,
}

#[derive(Serialize, Deserialize)]
/// Signed withdrawal transaction file ready for submission.
pub struct SignedWithdrawFile {
    /// File format version.
    pub version: u8,
    /// File kind discriminator.
    pub kind: String,
    /// Hex-encoded zone channel identifier.
    pub channel_id: String,
    /// Hex-encoded signed transaction hash.
    pub tx_hash: String,
    /// Hex-encoded inscription message identifier.
    pub msg_id: String,
    /// Hex-encoded signed mantle transaction bytes.
    pub signed_mantle_tx: String,
    /// Signature entries used to build the signed transaction.
    pub signatures: Vec<WithdrawSignatureEntry>,
}
