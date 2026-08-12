use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use lb_log_targets::mantle;
use tracing::info;

use super::{SDPActiveOp, SdpError};
use crate::{
    events::TxEvent,
    mantle::{
        Value,
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            Declarations, ExecutableOperation, PreverifiableOperation, ProvableOperation,
            VerifiableOperation, verification_mode, verification_mode::VerificationMode,
        },
        ops::SignedOperation,
        transactions::{hash::TxHashView, states::VerificationState},
    },
};

const LOG_TARGET: &str = mantle::sdp::message::ACTIVE;

pub struct SDPActiveValidationContext<'a> {
    pub declarations: &'a Declarations,
    pub tx_hash_view: &'a TxHashView,
    pub epoch: Epoch,
}

pub struct SDPActiveExecutionContext {
    pub epoch: Epoch,
    pub declarations: Declarations,
}

impl ProvableOperation for SDPActiveOp {
    type Proof = ZkSignature;
    const CODE: u8 = 0x22;
}

impl OperationGas<MainnetGasProfile> for SDPActiveOp {
    const GAS_COST: Gas = Gas::new(590);
}

impl PreverifiableOperation<verification_mode::StandardMode> for SDPActiveOp {
    type Context<'a> = ();
    type Error = SdpError;

    fn preverify(
        &self,
        _proof: &Self::Proof,
        _context: &Self::Context<'_>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl VerifiableOperation<verification_mode::StandardMode> for SDPActiveOp {
    type Context<'a> = SDPActiveValidationContext<'a>;
    type Error = SdpError;

    fn verify(&self, proof: &Self::Proof, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Check the declaration exists
        let Some(declaration) = context.declarations.get(&self.declaration_id) else {
            return Err(SdpError::DeclarationNotFound(self.declaration_id));
        };

        // Check the declaration hasn't been withdrawn
        // (Return error if `scheduled_withdrawal_epoch` epoch has passed)
        if let Some(withdraw_at) = declaration.withdraw_at
            && withdraw_at <= context.epoch
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
        if !ZkPublicKey::verify_multi(&[declaration.zk_id], context.tx_hash_view.as_fr(), proof) {
            return Err(SdpError::InvalidZkSignature);
        }

        Ok(())
    }
}

impl ExecutableOperation for SDPActiveOp {
    type Context<'a> = SDPActiveExecutionContext;
    type Error = SdpError;

    // TODO: check service specific logic
    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let declaration = context
            .declarations
            .get_mut(&self.declaration_id)
            .expect("The operation should have been validated");

        declaration.active = context.epoch;
        declaration.nonce = self.nonce;
        info!(
            target: LOG_TARGET,
            provider_id = ?declaration.provider_id,
            active = ?declaration.active,
            nonce = ?declaration.nonce,
            "updated declaration with active message"
        );

        Ok((context, Vec::new()))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<SDPActiveOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
    }
}
