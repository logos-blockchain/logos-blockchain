use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use tracing::info;

use super::{SDPWithdrawOp, SdpError};
use crate::{
    block::BlockNumber,
    mantle::{
        TxHash,
        ledger::{Declarations, Operation},
    },
    sdp::locked_notes::LockedNotes,
};

pub struct SDPWithdrawValidationContext<'a> {
    pub lock_period: &'a u64,
    pub declarations: &'a Declarations,
    pub block_number: &'a BlockNumber,
    pub locked_notes: &'a LockedNotes,
    pub tx_hash: &'a TxHash,
    pub sdp_withdraw_sig: &'a ZkSignature,
}

pub struct SDPWithdrawExecutionContext {
    pub block_number: BlockNumber,
    pub declarations: Declarations,
    pub locked_notes: LockedNotes,
}

impl Operation<SDPWithdrawValidationContext<'_>> for SDPWithdrawOp {
    type ExecutionContext<'a>
        = SDPWithdrawExecutionContext
    where
        Self: 'a;
    type Error = SdpError;

    fn validate(&self, ctx: &SDPWithdrawValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the declaration exists
        let Some(declaration) = ctx.declarations.get(&self.declaration_id) else {
            return Err(SdpError::DeclarationNotFound(self.declaration_id));
        };

        // Check that the locked note is locked for this service
        if !ctx
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

        // Check the note can be unlocked
        if declaration.created + ctx.lock_period >= *ctx.block_number {
            return Err(SdpError::WithdrawalWhileLocked);
        }

        // Ensure locked note pk and zk_id attached to this declaration authorized this
        // Operation.
        let note = ctx
            .locked_notes
            .get(&self.locked_note_id)
            .expect("The Operation has been checked above");
        if !ZkPublicKey::verify_multi(
            &[note.pk, declaration.zk_id],
            &ctx.tx_hash.to_fr(),
            ctx.sdp_withdraw_sig,
        ) {
            return Err(SdpError::InvalidZkSignature);
        }

        // Check that the nonce is greater than the previous one
        if self.nonce <= declaration.nonce {
            return Err(SdpError::InvalidNonce {
                message_nonce: self.nonce,
                declaration_nonce: declaration.nonce,
            });
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error> {
        let declaration = ctx
            .declarations
            .get(&self.declaration_id)
            .expect("The operation should have been validated");

        info!(
            provider_id = ?declaration.provider_id,
            nonce = self.nonce,
            "updated declaration with withdraw message"
        );

        let _ = ctx
            .locked_notes
            .unlock(declaration.service_type, &self.locked_note_id)
            .expect("The operation should have been validated");

        ctx.declarations = ctx.declarations.remove(&self.declaration_id);

        Ok(ctx)
    }
}
