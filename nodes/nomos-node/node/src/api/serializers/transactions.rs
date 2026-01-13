use key_management_system_service::keys::ZkSignature;
use nomos_core::mantle::{
    MantleTx, Op, OpProof, SignedMantleTx, TxHash, gas::Gas, ledger::Tx as LedgerTx,
};
use serde::Serialize;

#[derive(Serialize)]
#[serde(remote = "MantleTx")]
pub struct ApiTransactionSerializer {
    #[serde(getter = "<MantleTx as nomos_core::mantle::Transaction>::hash")]
    hash: TxHash,
    ops: Vec<Op>,
    ledger_tx: LedgerTx,
    execution_gas_price: Gas,
    storage_gas_price: Gas,
}

#[derive(Serialize)]
#[serde(remote = "SignedMantleTx")]
pub struct ApiSignedTransactionSerializer {
    #[serde(with = "ApiTransactionSerializer")]
    mantle_tx: MantleTx,
    ops_proofs: Vec<OpProof>,
    ledger_tx_proof: ZkSignature,
}

#[derive(serde::Serialize)]
struct SignedApiTransaction<'a>(
    #[serde(with = "ApiSignedTransactionSerializer")] &'a SignedMantleTx,
);

pub mod signed_api_transaction_vec {
    use nomos_core::mantle::SignedMantleTx;
    use serde::ser::SerializeSeq as _;

    use crate::api::serializers::transactions::SignedApiTransaction;

    pub fn serialize<Serializer>(
        value: &Vec<SignedMantleTx>,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(value.len()))?;
        for transaction in value {
            let signed_api_transaction = SignedApiTransaction(transaction);
            sequence.serialize_element(&signed_api_transaction)?;
        }
        sequence.end()
    }
}
