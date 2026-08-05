use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::ZkPublicKey;

use super::{SDPDeclareOp, SdpError};
use crate::{
    events::TxEvent,
    mantle::{
        Note, Value,
        channel::Channels,
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            Declarations, ExecutableOperation, PreverifiableOperation, ProvableOperation, Utxos,
            VerifiableOperation, verification_mode, verification_mode::VerificationMode,
        },
        ops::{SignedOp, ZkAndEd25519Proof},
        transactions::{hash::TxHashView, states::VerificationState},
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
        context: SDPDeclareExecutionContext,
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
        mut context: SDPDeclareExecutionContext,
    ) -> Result<(SDPDeclareExecutionContext, Vec<TxEvent>), SdpError> {
        let declaration_id = self.id();
        let declaration = Declaration::new(context.epoch, self);
        context.declarations = context.declarations.insert(declaration_id, declaration);
        let utxo = context
            .utxo_tree
            .utxos()
            .get(&self.locked_note_id)
            .expect("The operation should have been checked")
            .0;

        context.locked_notes = context
            .locked_notes
            .lock(
                &context.min_stake,
                self.service_type,
                utxo.note,
                &self.locked_note_id,
            )
            .map_err(|_| SdpError::UnexpectedError)?;

        Ok((context, Vec::new()))
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

pub struct SDPDeclarePreverificationContext<'a> {
    pub tx_hash_view: &'a TxHashView,
}

pub struct SDPDeclareVerificationContext<'a> {
    pub utxo_tree: &'a Utxos,
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub tx_hash_view: &'a TxHashView,
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

impl ProvableOperation for SDPDeclareOp {
    type Proof = ZkAndEd25519Proof;
}

impl OperationGas<MainnetGasProfile> for SDPDeclareOp {
    const GAS_CONSTANT: Gas = Gas::new(646);
}

impl PreverifiableOperation<verification_mode::StandardMode> for SDPDeclareOp {
    type Context<'a> = SDPDeclarePreverificationContext<'a>;
    type Error = SdpError;

    fn preverify(
        &self,
        proof: &Self::Proof,
        context: &Self::Context<'_>,
    ) -> Result<(), Self::Error> {
        self.preverify(context.tx_hash_view, &proof.ed25519_sig)
    }
}

impl VerifiableOperation<verification_mode::StandardMode> for SDPDeclareOp {
    type Context<'a> = SDPDeclareVerificationContext<'a>;
    type Error = SdpError;

    fn verify(&self, proof: &Self::Proof, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Check that the note exist
        let Some((utxo, _)) = context.utxo_tree.utxos().get(&self.locked_note_id) else {
            return Err(SdpError::InexistingNote(self.locked_note_id));
        };

        // Ensure locked note exists and ownership over the locked note and `zk_id`
        let note = utxo.note;
        if !ZkPublicKey::verify_multi(
            &[note.pk, self.zk_id],
            context.tx_hash_view.as_fr(),
            &proof.zk_sig,
        ) {
            return Err(SdpError::InvalidZkSignature);
        }

        SDPDeclareValidationExt::validate(
            self,
            note,
            context.channels,
            context.declarations,
            context.locked_notes,
            context.min_stake,
        )
    }
}

impl PreverifiableOperation<verification_mode::GenesisMode> for SDPDeclareOp {
    type Context<'a> = SDPDeclarePreverificationContext<'a>;
    type Error = SdpError;

    fn preverify(
        &self,
        proof: &Self::Proof,
        context: &Self::Context<'_>,
    ) -> Result<(), Self::Error> {
        self.preverify(context.tx_hash_view, &proof.ed25519_sig)
    }
}

impl VerifiableOperation<verification_mode::GenesisMode> for SDPDeclareOp {
    type Context<'a> = SDPDeclareGenesisValidationContext<'a>;
    type Error = SdpError;

    fn verify(&self, _proof: &Self::Proof, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Check that the note exist
        let Some((utxo, _)) = context.utxo_tree.utxos().get(&self.locked_note_id) else {
            return Err(SdpError::InexistingNote(self.locked_note_id));
        };
        let note = utxo.note;

        SDPDeclareValidationExt::validate(
            self,
            note,
            context.channels,
            context.declarations,
            context.locked_notes,
            context.min_stake,
        )
    }
}

impl ExecutableOperation for SDPDeclareOp {
    type Context<'a> = SDPDeclareExecutionContext;
    type Error = SdpError;

    fn execute<'a>(
        &self,
        context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        SDPDeclareValidationExt::execute(self, context)
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOp<SDPDeclareOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
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
