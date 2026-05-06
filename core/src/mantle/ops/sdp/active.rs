use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use tracing::info;

use super::{SDPActiveOp, SdpError};
use crate::{
    block::BlockNumber,
    mantle::{
        TxHash,
        ledger::{Declarations, Operation},
    },
};

pub struct SDPActiveValidationContext<'a> {
    pub declarations: &'a Declarations,
    pub tx_hash: &'a TxHash,
    pub active_sig: &'a ZkSignature,
}

pub struct SDPActiveExecutionContext {
    pub block_number: BlockNumber,
    pub declarations: Declarations,
}

impl Operation for SDPActiveOp {
    type ValidationContext<'a>
        = SDPActiveValidationContext<'a>
    where
        Self: 'a;
    type ExecutionContext<'a>
        = SDPActiveExecutionContext
    where
        Self: 'a;
    type Error = SdpError;

    fn validate(&self, ctx: &Self::ValidationContext<'_>) -> Result<(), Self::Error> {
        // Check the declaration exist
        let Some(declaration) = ctx.declarations.get(&self.declaration_id) else {
            return Err(SdpError::DeclarationNotFound(self.declaration_id));
        };

        // Check the nonce is increasing
        if self.nonce <= declaration.nonce {
            return Err(SdpError::InvalidNonce {
                message_nonce: self.nonce,
                declaration_nonce: declaration.nonce,
            });
        }

        // Check the signature over the `zk_id`
        if !ZkPublicKey::verify_multi(&[declaration.zk_id], &ctx.tx_hash.to_fr(), ctx.active_sig) {
            return Err(SdpError::InvalidZkSignature);
        }

        Ok(())
    }

    // TODO: check service specific logic
    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error> {
        let declaration = ctx
            .declarations
            .get_mut(&self.declaration_id)
            .expect("The operation should have been validated");

        declaration.active = ctx.block_number;
        declaration.nonce = self.nonce;
        info!(
            provider_id = ?declaration.provider_id,
            active = declaration.active,
            nonce = declaration.nonce,
            "updated declaration with active message"
        );

        Ok(ctx)
    }
}
