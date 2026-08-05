use std::marker::PhantomData;

use crate::{
    events::TxEvent,
    mantle::{
        GasProfile,
        batch::DeferredZkpVerification,
        gas::{Gas, OperationGas},
        ledger::{
            ExecutableOperation, Operation, PreverifiableOperation, ProvableOperation,
            VerifiableOperation, verification_mode::VerificationMode,
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

impl<T: PreverifiableOperation<Mode>, Mode: VerificationMode> SignedOperation<T, Unverified, Mode> {
    pub fn preverify(
        self,
        context: &T::Context<'_>,
    ) -> Result<SignedOperation<T, Preverified, Mode>, T::Error> {
        self.operation.preverify(&self.proof, context)?;
        Ok(self.into_state())
    }
}

impl<T: VerifiableOperation<Mode>, Mode: VerificationMode> SignedOperation<T, Preverified, Mode> {
    pub fn verify(
        self,
        context: &T::Context<'_>,
    ) -> Result<VerifiedSignedOperation<T, Mode>, (Self, T::Error)> {
        let verify_result = self.operation.verify(&self.proof, context);
        match verify_result {
            Ok(deferred_zkp) => Ok(VerifiedSignedOperation {
                signed_operation: self.into_state(),
                deferred_zkp,
            }),
            Err(error) => Err((self, error)),
        }
    }
}

impl<T: Operation<Mode>, Mode: VerificationMode> SignedOperation<T, Verified, Mode> {
    pub fn execute<'a>(
        &self,
        context: <T as ExecutableOperation>::Context<'a>,
    ) -> Result<
        (<T as ExecutableOperation>::Context<'a>, Vec<TxEvent>),
        <T as ExecutableOperation>::Error,
    > {
        self.operation.execute(context)
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

#[expect(dead_code, reason = "TODO: use SignedOp::verify")]
pub struct VerifiedSignedOperation<T: ProvableOperation, Mode: VerificationMode> {
    signed_operation: SignedOperation<T, Verified, Mode>,
    deferred_zkp: Option<DeferredZkpVerification>,
}
