use crate::mantle::{
    ledger::verification_mode::VerificationMode,
    transactions::{SignedOps, states::VerificationState},
};

pub trait SignedMantleTx<State: VerificationState, Mode: VerificationMode> {
    fn signed_ops(&self) -> &SignedOps<State, Mode>;
}
