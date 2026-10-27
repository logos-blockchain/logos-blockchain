use lb_key_management_system_keys::keys::ZkPublicKey;
use rpds::HashTrieMapSync;

use crate::{
    events::TxEvent,
    mantle::ledger::verification_mode::VerificationMode,
    sdp::{Declaration, DeclarationId, ProviderId, ServiceType},
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

/// The `provider_id` and the `zk_id` of each declaration of some
/// [`Declarations`], within its service, with the id of the declaration, so a
/// declaration finds the ones it would duplicate without scanning them.
///
/// Every declaration is checked unique in its service when declared, so each
/// key names a single declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeclarationKeys {
    provider_ids: HashTrieMapSync<(ServiceType, ProviderId), DeclarationId>,
    zk_ids: HashTrieMapSync<(ServiceType, ZkPublicKey), DeclarationId>,
}

impl DeclarationKeys {
    #[must_use]
    pub fn from_declarations(declarations: &Declarations) -> Self {
        let mut keys = Self::default();
        for (declaration_id, declaration) in declarations {
            keys.insert_mut(*declaration_id, declaration);
        }
        keys
    }

    pub fn insert_mut(&mut self, declaration_id: DeclarationId, declaration: &Declaration) {
        self.provider_ids.insert_mut(
            (declaration.service_type, declaration.provider_id),
            declaration_id,
        );
        self.zk_ids.insert_mut(
            (declaration.service_type, declaration.zk_id),
            declaration_id,
        );
    }

    pub fn remove_mut(&mut self, declaration: &Declaration) {
        self.provider_ids
            .remove_mut(&(declaration.service_type, declaration.provider_id));
        self.zk_ids
            .remove_mut(&(declaration.service_type, declaration.zk_id));
    }

    /// The declaration of `service_type` that holds `provider_id`.
    #[must_use]
    pub fn provider_id(
        &self,
        service_type: ServiceType,
        provider_id: ProviderId,
    ) -> Option<&DeclarationId> {
        self.provider_ids.get(&(service_type, provider_id))
    }

    /// The declaration of `service_type` that holds `zk_id`.
    #[must_use]
    pub fn zk_id(&self, service_type: ServiceType, zk_id: ZkPublicKey) -> Option<&DeclarationId> {
        self.zk_ids.get(&(service_type, zk_id))
    }
}

pub type Value = u64;
