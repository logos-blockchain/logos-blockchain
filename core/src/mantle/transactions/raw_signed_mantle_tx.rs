use crate::mantle::{
    ledger::verification_mode::VerificationMode,
    traits::signed_mantle_tx::SignedMantleTx,
    transactions::{SignedOps, states::VerificationState},
};

pub struct RawSignedMantleTx<State: VerificationState, Mode: VerificationMode>(
    SignedOps<State, Mode>,
);

impl<State: VerificationState, Mode: VerificationMode> RawSignedMantleTx<State, Mode> {
    #[must_use]
    pub const fn new(signed_ops: SignedOps<State, Mode>) -> Self {
        Self(signed_ops)
    }

    #[must_use]
    pub const fn inner(&self) -> &SignedOps<State, Mode> {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> SignedOps<State, Mode> {
        self.0
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedMantleTx<State, Mode>
    for RawSignedMantleTx<State, Mode>
{
    fn signed_ops(&self) -> &SignedOps<State, Mode> {
        self.inner()
    }
}
