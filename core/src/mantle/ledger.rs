use crate::{
    events::TxEvent,
    mantle::ledger::verification_mode::VerificationMode,
    sdp::{Declaration, DeclarationId, ProviderId},
};

pub mod verification_mode {
    pub trait VerificationMode {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct GenesisMode;
    impl VerificationMode for GenesisMode {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct StandardMode;
    impl VerificationMode for StandardMode {}
}

pub trait ProvableOperation {
    type Proof;
    const CODE: u8;
}

pub trait PreverifiableOperation<Mode: VerificationMode> {
    type Context<'a>;
    type Error;

    fn preverify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error>;
}

pub trait VerifiableOperation<Mode: VerificationMode> {
    /// The proof [`verify`](Self::verify) hands off for batch verification, or
    /// `()` if there is none.
    ///
    /// Unbounded on purpose:
    /// [`SignedOperation`](crate::mantle::ops::SignedOperation) only provides
    /// [`into_verified`](crate::mantle::ops::SignedOperation::into_verified)
    /// for `()` and
    /// [`into_verified_deferred`](crate::mantle::ops::SignedOperation::into_verified_deferred) for
    /// [`DeferrableProof`](crate::mantle::batch::DeferrableProof) types.
    type DeferredProof;
    type Context<'a>;
    type Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error>;
}

pub trait ExecutableOperation {
    type Context<'a>;
    type Error;

    fn execute<'a>(
        &self,
        context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error>;
}

pub type Declarations = rpds::RedBlackTreeMapSync<DeclarationId, Declaration>;
/// Per-service provider-to-declaration index used by SDP Declare validation.
pub type ProviderIndex = rpds::RedBlackTreeMapSync<ProviderId, DeclarationId>;

pub type Value = u64;
