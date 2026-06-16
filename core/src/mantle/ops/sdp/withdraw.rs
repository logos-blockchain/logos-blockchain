use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use lb_log_targets::mantle;
use tracing::debug;

use super::{SDPWithdrawOp, SdpError};
use crate::{
    events::Events,
    mantle::{
        TxHash,
        ledger::{Declarations, Operation},
    },
    sdp::{self, locked_notes::LockedNotes},
};

const LOG_TARGET: &str = mantle::sdp::message::WITHDRAW;

pub struct SDPWithdrawValidationContext<'a> {
    pub declarations: &'a Declarations,
    pub epoch: Epoch,
    pub locked_notes: &'a LockedNotes,
    pub tx_hash: &'a TxHash,
    pub sdp_withdraw_sig: &'a ZkSignature,
}

pub struct SDPWithdrawExecutionContext {
    pub declarations: Declarations,
    pub locked_notes: LockedNotes,
    pub epoch: Epoch,
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

        // Check that the declaration hasn't been withdrawn
        if let Some(withdrawn) = declaration.withdrawn {
            return Err(SdpError::DeclarationWithdrawn {
                declaration_id: self.declaration_id,
                withdrawn_epoch: withdrawn,
            });
        }

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
    ) -> Result<(Self::ExecutionContext<'_>, Events), Self::Error> {
        let declaration = ctx
            .declarations
            .get_mut(&self.declaration_id)
            .expect("The operation should have been validated");

        // Delay the withdrawal by `SNAPSHOT_FINALIZATION_DELAY` epochs
        // to prevent "stake-less service provision".
        // Otherwise, providers can continue providing the service even after
        // withdrawal because SDP uses the snapshot from `SNAPSHOT_FINALIZATION_DELAY`
        // epochs ago.
        // The note will be unlocked once the withdrawn epoch set here is reached.
        declaration.withdrawn = Some(ctx.epoch.strict_add(sdp::SNAPSHOT_FINALIZATION_DELAY));
        declaration.nonce = self.nonce;

        debug!(
            target: LOG_TARGET,
            provider_id = ?declaration.provider_id,
            withdrawn = ?declaration.withdrawn,
            nonce = ?declaration.nonce,
            "updated declaration with withdraw message"
        );

        Ok((ctx, Events::new()))
    }
}
