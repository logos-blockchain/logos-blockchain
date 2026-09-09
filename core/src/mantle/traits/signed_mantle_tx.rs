use crate::mantle::{
    ledger::verification_mode::VerificationMode,
    traits::MantleTx,
    transactions::{OpProofRefs, SignedOps, states::VerificationState},
};

pub trait SignedMantleTx<State: VerificationState, Mode: VerificationMode>: MantleTx {
    fn signed_ops(&self) -> &SignedOps<State, Mode>;

    fn op_proof_refs(&self) -> OpProofRefs<'_>;
}
