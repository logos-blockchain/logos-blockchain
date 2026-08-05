use lb_core::mantle::{
    MantleTransaction, TxHash,
    traits::Hashable,
    transactions::{Ops, OpsProofs, mantle_tx::MantleTx, states::VerificationState},
};
use serde::Serialize;

#[derive(Serialize)]
pub struct ApiTransactionSerializer<'tx> {
    hash: TxHash,
    ops: &'tx Ops,
}

impl<'tx, T> From<&'tx T> for ApiTransactionSerializer<'tx>
where
    T: MantleTx + Hashable<Hash = TxHash>,
{
    fn from(tx: &'tx T) -> Self {
        Self {
            hash: tx.hash(),
            ops: tx.ops(),
        }
    }
}

#[derive(Serialize)]
pub struct ApiSignedTransaction<'tx> {
    mantle_tx: ApiTransactionSerializer<'tx>,
    ops_proofs: &'tx OpsProofs,
}

impl<'tx, State: VerificationState> From<&'tx MantleTransaction<State>>
    for ApiSignedTransaction<'tx>
{
    fn from(value: &'tx MantleTransaction<State>) -> Self {
        Self {
            mantle_tx: value.mantle_tx().into(),
            ops_proofs: value.ops_proofs(),
        }
    }
}
