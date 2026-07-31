use lb_core::mantle::{
    MantleTx, SignedMantleTx, TxHash,
    transactions::{Ops, OpsProofs, states::VerificationState},
};
use serde::Serialize;

#[derive(Serialize)]
#[serde(remote = "MantleTx")]
pub struct ApiTransactionSerializer {
    #[serde(getter = "<MantleTx as lb_core::mantle::traits::Hashable>::hash")]
    hash: TxHash,
    #[serde(getter = "MantleTx::ops")]
    ops: Ops,
}

#[derive(Serialize)]
pub struct ApiSignedTransaction<'tx> {
    #[serde(with = "ApiTransactionSerializer")]
    mantle_tx: &'tx MantleTx,
    ops_proofs: &'tx OpsProofs,
}

impl<'tx, State: VerificationState> From<&'tx SignedMantleTx<State>> for ApiSignedTransaction<'tx> {
    fn from(value: &'tx SignedMantleTx<State>) -> Self {
        Self {
            mantle_tx: value.mantle_tx(),
            ops_proofs: value.ops_proofs(),
        }
    }
}
