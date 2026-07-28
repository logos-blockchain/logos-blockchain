use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use lb_log_targets::mantle;
use tracing::info;

use super::{SDPActiveOp, SdpError};
use crate::{
    events::TxEvent,
    mantle::{
        ledger::{Declarations, Operation},
        transactions::hash::TxHashView,
    },
};

const LOG_TARGET: &str = mantle::sdp::message::ACTIVE;

pub struct SDPActiveValidationContext<'a> {
    pub declarations: &'a Declarations,
    pub tx_hash_view: &'a TxHashView,
    pub proof: &'a ZkSignature,
    pub epoch: Epoch,
}

pub struct SDPActiveExecutionContext {
    pub epoch: Epoch,
    pub declarations: Declarations,
}

impl Operation<SDPActiveValidationContext<'_>> for SDPActiveOp {
    type PreverificationContext<'a>
        = ()
    where
        Self: 'a;
    type ExecutionContext<'a>
        = SDPActiveExecutionContext
    where
        Self: 'a;
    type VerificationError = SdpError;
    type ExecutionError = SdpError;

    fn preverify(
        &self,
        _context: &Self::PreverificationContext<'_>,
    ) -> Result<(), Self::VerificationError> {
        Ok(())
    }

    fn verify(&self, ctx: &SDPActiveValidationContext<'_>) -> Result<(), Self::ExecutionError> {
        // Check the declaration exists
        let Some(declaration) = ctx.declarations.get(&self.declaration_id) else {
            return Err(SdpError::DeclarationNotFound(self.declaration_id));
        };

        // Check the declaration hasn't been withdrawn
        // (Return error if `scheduled_withdrawal_epoch` epoch has passed)
        if let Some(withdraw_at) = declaration.withdraw_at
            && withdraw_at <= ctx.epoch
        {
            return Err(SdpError::DeclarationWithdrawn {
                declaration_id: self.declaration_id,
                withdraw_at,
            });
        }

        // Check the nonce is increasing
        if self.nonce <= declaration.nonce {
            return Err(SdpError::InvalidNonce {
                message_nonce: self.nonce,
                declaration_nonce: declaration.nonce,
            });
        }

        // Check the signature over the `zk_id`
        if !ZkPublicKey::verify_multi(&[declaration.zk_id], ctx.tx_hash_view.as_fr(), ctx.proof) {
            return Err(SdpError::InvalidZkSignature);
        }

        Ok(())
    }

    // TODO: check service specific logic
    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<(Self::ExecutionContext<'_>, Vec<TxEvent>), Self::ExecutionError> {
        let mut declaration = ctx
            .declarations
            .get(&self.declaration_id)
            .expect("The operation should have been validated");

        declaration.active = ctx.epoch;
        declaration.nonce = self.nonce;
        ctx.declarations = ctx
            .declarations
            .update(&self.declaration_id, declaration.clone())
            .expect("the declaration is in the tree");
        info!(
            target: LOG_TARGET,
            provider_id = ?declaration.provider_id,
            active = ?declaration.active,
            nonce = ?declaration.nonce,
            "updated declaration with active message"
        );

        Ok((ctx, Vec::new()))
    }
}
