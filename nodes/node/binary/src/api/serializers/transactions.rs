use lb_core::mantle::{
    SignedOps, TxHash,
    ledger::verification_mode::VerificationMode,
    traits::{Hashable, MantleTx},
    transactions::{OpProofRefs, OpRefs, states::VerificationState},
};
use serde::Serialize;

#[derive(Serialize)]
pub struct ApiTransactionSerializer<'tx> {
    hash: TxHash,
    ops: OpRefs<'tx>,
}

impl<'tx, T> From<&'tx T> for ApiTransactionSerializer<'tx>
where
    T: MantleTx + Hashable<Hash = TxHash>,
{
    fn from(tx: &'tx T) -> Self {
        Self {
            hash: tx.hash(),
            ops: tx.op_refs(),
        }
    }
}

#[derive(Serialize)]
pub struct ApiSignedTransaction<'tx> {
    mantle_tx: ApiTransactionSerializer<'tx>,
    ops_proofs: OpProofRefs<'tx>,
}

impl<'tx, State: VerificationState, Mode: VerificationMode> From<&'tx SignedOps<State, Mode>>
    for ApiSignedTransaction<'tx>
{
    fn from(value: &'tx SignedOps<State, Mode>) -> Self {
        Self {
            mantle_tx: value.into(),
            ops_proofs: value.op_proof_refs(),
        }
    }
}
