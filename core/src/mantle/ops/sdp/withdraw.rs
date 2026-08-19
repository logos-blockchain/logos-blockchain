use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{ZkSignature, public_inputs_from_pks};
use lb_log_targets::mantle;
use tracing::debug;

use super::{SDPWithdrawOp, SdpError};
use crate::{
    events::TxEvent,
    mantle::{
        Value,
        batch::DeferredZkpVerification,
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            Declarations, ExecutableOperation, PreverifiableOperation, ProvableOperation,
            VerifiableOperation,
            verification_mode::{self, VerificationMode},
        },
        ops::SignedOp,
        transactions::{hash::TxHashView, states::VerificationState},
    },
    sdp::{self, locked_notes::LockedNotes},
};

const LOG_TARGET: &str = mantle::sdp::message::WITHDRAW;

pub struct SDPWithdrawValidationContext<'a> {
    pub declarations: &'a Declarations,
    pub epoch: Epoch,
    pub locked_notes: &'a LockedNotes,
    pub tx_hash_view: &'a TxHashView,
}

pub struct SDPWithdrawExecutionContext {
    pub declarations: Declarations,
    pub locked_notes: LockedNotes,
    pub epoch: Epoch,
}

impl ProvableOperation for SDPWithdrawOp {
    type Proof = ZkSignature;
}

impl OperationGas<MainnetGasProfile> for SDPWithdrawOp {
    const GAS_COST: Gas = Gas::new(590);
}

impl PreverifiableOperation<verification_mode::StandardMode> for SDPWithdrawOp {
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

impl VerifiableOperation<verification_mode::StandardMode> for SDPWithdrawOp {
    type Context<'a> = SDPWithdrawValidationContext<'a>;
    type Error = SdpError;

    fn verify(
        &self,
        proof: &Self::Proof,
        context: &Self::Context<'_>,
    ) -> Result<Option<DeferredZkpVerification>, Self::Error> {
        // Check that the declaration exists
        let Some(declaration) = context.declarations.get(&self.declaration_id) else {
            return Err(SdpError::DeclarationNotFound(self.declaration_id));
        };

        // Check that the declaration hasn't been already scheduled to be withdrawn.
        if let Some(withdraw_at) = declaration.withdraw_at {
            return Err(SdpError::DeclarationWithdrawn {
                declaration_id: self.declaration_id,
                withdraw_at,
            });
        }

        // Check that the locked note is locked for this service
        if !context
            .locked_notes
            .is_locked_for_service(&self.locked_note_id, &declaration.service_type)
        {
            return Err(SdpError::NoteNotLockedForService {
                note_id: self.locked_note_id,
                service_type: declaration.service_type,
            });
        }

        // Check that the locked note exist (it corresponds to the declaration locked
        // note)
        if declaration.locked_note_id != self.locked_note_id {
            return Err(SdpError::InvalidLockedNote {
                note_id: self.locked_note_id,
                expected: declaration.locked_note_id,
            });
        }

        // Check that the nonce is greater than the previous one
        if self.nonce <= declaration.nonce {
            return Err(SdpError::InvalidNonce {
                message_nonce: self.nonce,
                declaration_nonce: declaration.nonce,
            });
        }

        // Defer the proof verification, so that the caller can batch it.
        // Ensure locked note pk and zk_id attached to this declaration authorized this
        // Operation.
        let note = context
            .locked_notes
            .get(&self.locked_note_id)
            .expect("The Operation has been checked above");
        let inputs = public_inputs_from_pks(
            (*context.tx_hash_view.as_fr()).into(),
            &[note.pk, declaration.zk_id],
        )
        .map_err(|_| SdpError::InvalidZkSignature)?;
        Ok(Some(DeferredZkpVerification::ZkSig(
            *proof.as_proof(),
            inputs,
        )))
    }
}

impl ExecutableOperation for SDPWithdrawOp {
    type Context<'a> = SDPWithdrawExecutionContext;
    type Error = SdpError;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let declaration = context
            .declarations
            .get_mut(&self.declaration_id)
            .expect("The operation should have been validated");

        // Delay the withdrawal by `SNAPSHOT_FINALIZATION_DELAY` epochs
        // to prevent "stake-less service provision".
        // Otherwise, providers can continue providing the service even after
        // withdrawal because SDP uses the snapshot from `SNAPSHOT_FINALIZATION_DELAY`
        // epochs ago.
        // The note will be unlocked once the withdrawn epoch set here is reached.
        declaration.withdraw_at = Some(context.epoch.strict_add(sdp::SNAPSHOT_FINALIZATION_DELAY));
        declaration.nonce = self.nonce;

        debug!(
            target: LOG_TARGET,
            provider_id = ?declaration.provider_id,
            withdraw_at = ?declaration.withdraw_at,
            nonce = ?declaration.nonce,
            "updated declaration with withdraw message"
        );

        Ok((context, Vec::new()))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOp<SDPWithdrawOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
    }
}
