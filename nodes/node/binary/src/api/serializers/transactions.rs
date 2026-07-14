use lb_core::mantle::{
    MantleTx, SignedMantleTx, TxHash, transactions::{Ops, states::VerificationState},
};
use serde::Serialize;
use lb_core::mantle::transactions::tx::OpsProofs;

#[derive(Serialize)]
#[serde(remote = "MantleTx")]
pub struct ApiTransactionSerializer {
    #[serde(getter = "<MantleTx as lb_core::mantle::Transaction>::hash")]
    hash: TxHash,
    #[serde(getter = "MantleTx::ops")]
    ops: Ops,
}

#[derive(Serialize)]
#[serde(remote = "SignedMantleTx")]
pub struct ApiSignedTransactionSerializer<State: VerificationState> {
    #[serde(
        getter = "SignedMantleTx::mantle_tx",
        with = "ApiTransactionSerializer"
    )]
    pub(crate) mantle_tx: MantleTx,
    #[serde(getter = "SignedMantleTx::ops_proofs")]
    pub(crate) ops_proofs: OpsProofs,
    #[serde(skip)]
    state: State,
}

#[derive(Serialize)]
pub struct ApiSignedTransactionRef<'a, State: VerificationState>(
    #[serde(with = "ApiSignedTransactionSerializer")] &'a SignedMantleTx<State>,
);

impl<'a, State: VerificationState> From<&'a SignedMantleTx<State>>
    for ApiSignedTransactionRef<'a, State>
{
    fn from(value: &'a SignedMantleTx<State>) -> Self {
        Self(value)
    }
}

pub mod signed_api_transaction_vec {
    use lb_core::mantle::{SignedMantleTx, transactions::states::VerificationState};
    use serde::ser::SerializeSeq as _;

    use crate::api::serializers::transactions::ApiSignedTransactionRef;

    pub fn serialize<State: VerificationState, Serializer>(
        value: &Vec<SignedMantleTx<State>>,
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

pub mod adapters {
    use lb_core::mantle::{SignedMantleTx, transactions::states::VerificationState};
    use serde::Serialize;

    #[derive(Serialize)]
    pub struct ApiTransactionsRefAdapter<'a, State: VerificationState>(
        #[serde(with = "super::signed_api_transaction_vec")] pub &'a Vec<SignedMantleTx<State>>,
    );
}
