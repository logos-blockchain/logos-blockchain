use std::marker::PhantomData;

use crate::{
    events::TxEvent,
    mantle::{
        GasProfile,
        gas::{Gas, OperationGas},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::VerificationMode,
        },
        transactions::states::{Preverified, Unverified, VerificationState, Verified},
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedOperation<T: ProvableOperation, State: VerificationState, Mode: VerificationMode> {
    operation: T,
    proof: T::Proof,
    _marker: PhantomData<(State, Mode)>,
}

impl<T: ProvableOperation, State: VerificationState, Mode: VerificationMode>
    SignedOperation<T, State, Mode>
{
    #[must_use]
    fn into_state<NewState: VerificationState>(self) -> SignedOperation<T, NewState, Mode> {
        let Self {
            operation,
            proof,
            _marker,
        } = self;

        SignedOperation::<T, NewState, Mode> {
            operation,
            proof,
            _marker: PhantomData,
        }
    }

    #[must_use]
    pub const fn operation(&self) -> &T {
        &self.operation
    }

    #[must_use]
    pub const fn proof(&self) -> &T::Proof {
        &self.proof
    }

    /// Converts a `SignedOperation<T, State, Mode>` into a
    /// `SignedOperation<T, NewState, Mode>` without performing any
    /// verification.
    ///
    /// This function is intended for
    /// [`GenesisTx`](crate::mantle::transactions::genesis_tx::GenesisTx) and
    /// testing purposes only.
    #[must_use]
    #[doc(hidden)]
    pub fn into_state_trusted<NewState: VerificationState>(
        self,
    ) -> SignedOperation<T, NewState, Mode> {
        self.into_state()
    }
}

impl<T: ProvableOperation, Mode: VerificationMode> SignedOperation<T, Unverified, Mode> {
    #[must_use]
    pub const fn new(operation: T, proof: T::Proof) -> Self {
        Self {
            operation,
            proof,
            _marker: PhantomData,
        }
    }
}

impl<T: ProvableOperation, Mode: VerificationMode> SignedOperation<T, Unverified, Mode>
where
    Self: PreverifiableOperation<Mode>,
{
    pub fn into_preverified(
        self,
        context: &<Self as PreverifiableOperation<Mode>>::Context<'_>,
    ) -> Result<SignedOperation<T, Preverified, Mode>, <Self as PreverifiableOperation<Mode>>::Error>
    {
        self.preverify(context)?;
        Ok(self.into_state())
    }
}

pub type VerifyError<T, Mode> = (
    SignedOperation<T, Preverified, Mode>,
    <SignedOperation<T, Preverified, Mode> as VerifiableOperation<Mode>>::Error,
);

impl<T: ProvableOperation, Mode: VerificationMode> SignedOperation<T, Preverified, Mode>
where
    Self: VerifiableOperation<Mode>,
{
    pub fn into_verified(
        self,
        context: &<Self as VerifiableOperation<Mode>>::Context<'_>,
    ) -> Result<SignedOperation<T, Verified, Mode>, VerifyError<T, Mode>> {
        let verify_result = self.verify(context);
        match verify_result {
            Ok(()) => Ok(self.into_state()),
            Err(error) => Err((self, error)),
        }
    }
}

pub type ExecuteOk<'context, T, Mode> = (
    <SignedOperation<T, Verified, Mode> as ExecutableOperation>::Context<'context>,
    Vec<TxEvent>,
);
pub type ExecuteError<T, Mode> = (
    SignedOperation<T, Verified, Mode>,
    <SignedOperation<T, Verified, Mode> as ExecutableOperation>::Error,
);

impl<T: ProvableOperation, Mode: VerificationMode> SignedOperation<T, Verified, Mode>
where
    Self: ExecutableOperation,
{
    pub fn execute(
        self,
        context: <Self as ExecutableOperation>::Context<'_>,
    ) -> Result<ExecuteOk<'_, T, Mode>, ExecuteError<T, Mode>> {
        <Self as ExecutableOperation>::execute(&self, context).map_err(|error| (self, error))
    }
}

impl<Profile, T, State, Mode> OperationGas<Profile> for SignedOperation<T, State, Mode>
where
    Profile: GasProfile,
    T: OperationGas<Profile> + ProvableOperation,
    State: VerificationState,
    Mode: VerificationMode,
{
    const GAS_COST: Gas = T::GAS_COST;
}
