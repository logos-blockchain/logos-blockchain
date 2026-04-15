use lb_core::mantle::{MantleTx, Op, OpProof, SignedMantleTx, TxHash, gas::GasPrice};
use serde::Serialize;

#[derive(Serialize)]
#[serde(remote = "MantleTx")]
pub struct ApiTransactionSerializer {
    #[serde(getter = "<MantleTx as lb_core::mantle::Transaction>::hash")]
    hash: TxHash,
    ops: Vec<Op>,
    execution_gas_price: GasPrice,
    storage_gas_price: GasPrice,
}

#[derive(Serialize)]
#[serde(remote = "SignedMantleTx")]
pub struct ApiSignedTransactionSerializer {
    #[serde(with = "ApiTransactionSerializer")]
    mantle_tx: MantleTx,
    ops_proofs: Vec<OpProof>,
}

#[derive(Serialize)]
pub struct ApiSignedTransactionRef<'a>(
    #[serde(with = "ApiSignedTransactionSerializer")] &'a SignedMantleTx,
);

impl<'a> From<&'a SignedMantleTx> for ApiSignedTransactionRef<'a> {
    fn from(value: &'a SignedMantleTx) -> Self {
        Self(value)
    }
}

pub mod signed_api_transaction_vec {
    use lb_core::mantle::SignedMantleTx;
    use serde::ser::SerializeSeq as _;

    use crate::api::serializers::transactions::ApiSignedTransactionRef;

    pub fn serialize<Serializer>(
        value: &Vec<SignedMantleTx>,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(value.len()))?;
        for transaction in value {
            let signed_api_transaction = ApiSignedTransactionRef(transaction);
            sequence.serialize_element(&signed_api_transaction)?;
        }
        sequence.end()
    }
}
