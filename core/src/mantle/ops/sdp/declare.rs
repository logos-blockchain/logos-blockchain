use lb_key_management_system_keys::keys::{Ed25519Signature, ZkPublicKey, ZkSignature};

use super::{MAX_DECLARATION_LOCATOR, SDPDeclareOp, SdpError};
use crate::{
    mantle::{
        TxHash,
        ledger::{Declarations, Operation, Utxos},
    },
    sdp::{Declaration, MinStake, locked_notes::LockedNotes},
};

pub struct SDPDeclareValidationContext<'a> {
    pub utxo_tree: &'a Utxos,
    pub locked_notes: &'a LockedNotes,
    pub tx_hash: &'a TxHash,
    pub declare_zk_sig: &'a ZkSignature,
    pub declare_eddsa_sig: &'a Ed25519Signature,
    pub declarations: &'a Declarations,
    pub min_stake: &'a MinStake,
}

pub struct SDPDeclareExecutionContext {
    pub utxo_tree: Utxos,
    pub block_number: u64,
    pub declarations: Declarations,
    pub locked_notes: LockedNotes,
    pub min_stake: MinStake,
}

impl Operation for SDPDeclareOp {
    type ValidationContext<'a>
        = SDPDeclareValidationContext<'a>
    where
        Self: 'a;
    type ExecutionContext<'a>
        = SDPDeclareExecutionContext
    where
        Self: 'a;
    type Error = SdpError;

    fn validate(&self, ctx: &Self::ValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the note exist
        let Some((utxo, _)) = ctx.utxo_tree.utxos().get(&self.locked_note_id) else {
            return Err(SdpError::InexistingNote(self.locked_note_id));
        };

        // Ensure locked note exists and ownership over the locked note and `zk_id`
        let note = utxo.note;
        if !ZkPublicKey::verify_multi(&[note.pk, self.zk_id], &ctx.tx_hash.0, ctx.declare_zk_sig) {
            return Err(SdpError::InvalidZkSignature);
        }

        // Ensure ownership over the `provider_id`
        self.provider_id
            .0
            .verify(
                ctx.tx_hash.as_signing_bytes().as_ref(),
                ctx.declare_eddsa_sig,
            )
            .map_err(|_| SdpError::InvalidEddsaSignature)?;

        // Check that the declaration doesn't already exist
        if ctx.declarations.contains_key(&self.id()) {
            return Err(SdpError::DuplicateDeclaration(self.id()));
        }

        // Ensure it has no more than 8 locators.
        if self.locators.len() > MAX_DECLARATION_LOCATOR {
            return Err(SdpError::TooMuchLocators);
        }

        // Ensure value of locked note is sufficient for joining the service.
        if note.value < ctx.min_stake.threshold {
            return Err(SdpError::NoteInsufficientValue {
                note_id: self.locked_note_id,
                value: note.value,
            });
        }

        // Ensure the note has not already been locked for this service.
        if ctx
            .locked_notes
            .is_locked_for_service(&self.locked_note_id, &self.service_type)
        {
            return Err(SdpError::NoteAlreadyUsedForService {
                note_id: self.locked_note_id,
                service_type: self.service_type,
            });
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error> {
        let declaration_id = self.id();
        let declaration = Declaration::new(ctx.block_number, self);
        ctx.declarations = ctx.declarations.insert(declaration_id, declaration);
        let utxo = ctx
            .utxo_tree
            .utxos()
            .get(&self.locked_note_id)
            .expect("The operation should have been checked")
            .0;

        ctx.locked_notes = ctx
            .locked_notes
            .lock(
                &ctx.min_stake,
                self.service_type,
                utxo.note,
                &self.locked_note_id,
            )
            .map_err(|_| SdpError::UnexpectedError)?;

        Ok(ctx)
    }
}
