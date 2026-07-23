use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkPublicKey, ZkSignature};

use super::{SDPDeclareOp, SdpError};
use crate::{
    events::TxEvent,
    mantle::{
        Note, TxHash,
        channel::Channels,
        ledger::{Declarations, Operation, Utxos},
    },
    sdp::{Declaration, MinStake, locked_notes::LockedNotes},
};

trait SDPDeclareValidationExt {
    fn validate(
        &self,
        note: Note,
        channels: &Channels,
        declarations: &Declarations,
        locked_notes: &LockedNotes,
        min_stake: &MinStake,
    ) -> Result<(), SdpError>;

    fn execute(
        &self,
        ctx: SDPDeclareExecutionContext,
    ) -> Result<(SDPDeclareExecutionContext, Vec<TxEvent>), SdpError>;
}

impl SDPDeclareValidationExt for SDPDeclareOp {
    fn validate(
        &self,
        note: Note,
        channels: &Channels,
        declarations: &Declarations,
        locked_notes: &LockedNotes,
        min_stake: &MinStake,
    ) -> Result<(), SdpError> {
        // Check that the declaration doesn't already exist
        if declarations.contains_key(&self.id()) {
            return Err(SdpError::DuplicateDeclaration(self.id()));
        }
        validate_service_scoped_uniqueness(self, declarations)?;

        // A channel note cannot be used as collateral for a service declaration.
        if channels.is_channel_note(&self.locked_note_id) {
            return Err(SdpError::ChannelNote(self.locked_note_id));
        }

        // Ensure value of locked note is sufficient for joining the service.
        if note.value < min_stake.threshold {
            return Err(SdpError::NoteInsufficientValue {
                note_id: self.locked_note_id,
                value: note.value,
            });
        }

        // Ensure the note has not already been locked for this service.
        if locked_notes.is_locked_for_service(&self.locked_note_id, &self.service_type) {
            return Err(SdpError::NoteAlreadyUsedForService {
                note_id: self.locked_note_id,
                service_type: self.service_type,
            });
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: SDPDeclareExecutionContext,
    ) -> Result<(SDPDeclareExecutionContext, Vec<TxEvent>), SdpError> {
        let declaration_id = self.id();
        let declaration = Declaration::new(ctx.epoch, self);
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

        Ok((ctx, Vec::new()))
    }
}

/// `provider_id` and `zk_id` must each be unique within the same service.
fn validate_service_scoped_uniqueness(
    op: &SDPDeclareOp,
    declarations: &Declarations,
) -> Result<(), SdpError> {
    declarations
        .values()
        .filter(|d| d.service_type == op.service_type)
        .try_for_each(|existing| {
            if existing.provider_id == op.provider_id {
                Err(SdpError::DuplicateProviderId {
                    service_type: op.service_type,
                    provider_id: Box::new(op.provider_id),
                })
            } else if existing.zk_id == op.zk_id {
                Err(SdpError::DuplicateZkId {
                    service_type: op.service_type,
                    zk_id: op.zk_id,
                })
            } else {
                Ok(())
            }
        })
}

pub struct SDPDeclareValidationContext<'a> {
    pub utxo_tree: &'a Utxos,
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub tx_hash: &'a TxHash,
    pub declare_zk_sig: &'a ZkSignature,
    pub declare_eddsa_sig: &'a Ed25519Signature,
    pub declarations: &'a Declarations,
    pub min_stake: &'a MinStake,
}

pub struct SDPDeclareGenesisValidationContext<'a> {
    pub utxo_tree: &'a Utxos,
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub declarations: &'a Declarations,
    pub min_stake: &'a MinStake,
}

pub struct SDPDeclareExecutionContext {
    pub utxo_tree: Utxos,
    pub epoch: Epoch,
    pub declarations: Declarations,
    pub locked_notes: LockedNotes,
    pub min_stake: MinStake,
}

impl Operation<SDPDeclareValidationContext<'_>> for SDPDeclareOp {
    type ExecutionContext<'a>
        = SDPDeclareExecutionContext
    where
        Self: 'a;
    type Error = SdpError;

    fn validate(&self, ctx: &SDPDeclareValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the note exist
        let Some((utxo, _)) = ctx.utxo_tree.utxos().get(&self.locked_note_id) else {
            return Err(SdpError::InexistingNote(self.locked_note_id));
        };

        // Ensure locked note exists and ownership over the locked note and `zk_id`
        let note = utxo.note;
        if !ZkPublicKey::verify_multi(
            &[note.pk, self.zk_id],
            &ctx.tx_hash.to_fr(),
            ctx.declare_zk_sig,
        ) {
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

        SDPDeclareValidationExt::validate(
            self,
            note,
            ctx.channels,
            ctx.declarations,
            ctx.locked_notes,
            ctx.min_stake,
        )
    }

    fn execute(
        &self,
        ctx: Self::ExecutionContext<'_>,
    ) -> Result<(Self::ExecutionContext<'_>, Vec<TxEvent>), Self::Error> {
        SDPDeclareValidationExt::execute(self, ctx)
    }
}

impl Operation<SDPDeclareGenesisValidationContext<'_>> for SDPDeclareOp {
    type ExecutionContext<'a>
        = SDPDeclareExecutionContext
    where
        Self: 'a;
    type Error = SdpError;

    fn validate(&self, ctx: &SDPDeclareGenesisValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the note exist
        let Some((utxo, _)) = ctx.utxo_tree.utxos().get(&self.locked_note_id) else {
            return Err(SdpError::InexistingNote(self.locked_note_id));
        };
        let note = utxo.note;

        SDPDeclareValidationExt::validate(
            self,
            note,
            ctx.channels,
            ctx.declarations,
            ctx.locked_notes,
            ctx.min_stake,
        )
    }

    fn execute(
        &self,
        ctx: Self::ExecutionContext<'_>,
    ) -> Result<(Self::ExecutionContext<'_>, Vec<TxEvent>), Self::Error> {
        SDPDeclareValidationExt::execute(self, ctx)
    }
}

#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;

    use super::{SDPDeclareOp, SdpError, validate_service_scoped_uniqueness};
    use crate::{
        mantle::ledger::Declarations,
        sdp::{Declaration, ServiceType},
    };

    fn declare_op(provider_sk: u8, zk_sk: u64, locator: &str) -> SDPDeclareOp {
        SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: vec![locator.parse().unwrap()].try_into().unwrap(),
            provider_id: Ed25519Key::from_bytes(&[provider_sk; 32])
                .public_key()
                .into(),
            zk_id: ZkKey::from(BigUint::from(zk_sk)).to_public_key(),
            locked_note_id: Fr::ZERO.into(),
        }
    }

    /// Two declarations in the same service sharing the same `provider_id`
    /// (different `zk_id` and locators) must be rejected by the SDP
    /// per-service uniqueness check.
    #[test]
    fn rejects_duplicate_provider_id_within_service() {
        let declare_a = declare_op(1, 1, "/ip4/1.1.1.1/udp/0");
        let declare_b = declare_op(1, 2, "/ip4/2.2.2.2/udp/0");

        let declarations = Declarations::new_sync()
            .insert(declare_a.id(), Declaration::new(Epoch::new(0), &declare_a));

        assert!(matches!(
            validate_service_scoped_uniqueness(&declare_b, &declarations),
            Err(SdpError::DuplicateProviderId { .. })
        ));
    }

    /// Two declarations in the same service sharing the same `zk_id`
    /// (different `provider_id` and locators) must be rejected by the SDP
    /// per-service uniqueness check.
    #[test]
    fn rejects_duplicate_zk_id_within_service() {
        let declare_a = declare_op(1, 1, "/ip4/1.1.1.1/udp/0");
        let declare_b = declare_op(2, 1, "/ip4/2.2.2.2/udp/0");

        let declarations = Declarations::new_sync()
            .insert(declare_a.id(), Declaration::new(Epoch::new(0), &declare_a));

        assert!(matches!(
            validate_service_scoped_uniqueness(&declare_b, &declarations),
            Err(SdpError::DuplicateZkId { .. })
        ));
    }
}
