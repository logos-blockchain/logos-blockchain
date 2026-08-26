use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use lb_log_targets::mantle;
use tracing::debug;

use super::{SDPWithdrawOp, SdpError};
use crate::{
    events::TxEvent,
    mantle::{
        Value,
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            Declarations, ExecutableOperation, PreverifiableOperation, ProvableOperation,
            VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::SignedOperation,
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, VerificationState, Verified},
        },
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
    const CODE: u8 = 0x21;
}

impl OperationGas<MainnetGasProfile> for SDPWithdrawOp {
    const GAS_COST: Gas = Gas::new(590);
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<SDPWithdrawOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = SdpError;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<SDPWithdrawOp, Preverified, StandardMode>
{
    type Context<'a> = SDPWithdrawValidationContext<'a>;
    type Error = SdpError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Check that the declaration exists
        let Some(declaration) = context.declarations.get(&operation.declaration_id) else {
            return Err(SdpError::DeclarationNotFound(operation.declaration_id));
        };

        // Check that the declaration hasn't been already scheduled to be withdrawn.
        if let Some(withdraw_at) = declaration.withdraw_at {
            return Err(SdpError::DeclarationWithdrawn {
                declaration_id: operation.declaration_id,
                withdraw_at,
            });
        }

        // Check that the locked note is locked for this service
        if !context
            .locked_notes
            .is_locked_for_service(&operation.locked_note_id, &declaration.service_type)
        {
            return Err(SdpError::NoteNotLockedForService {
                note_id: operation.locked_note_id,
                service_type: declaration.service_type,
            });
        }

        // Check that the locked note exist (it corresponds to the declaration locked
        // note)
        if declaration.locked_note_id != operation.locked_note_id {
            return Err(SdpError::InvalidLockedNote {
                note_id: operation.locked_note_id,
                expected: declaration.locked_note_id,
            });
        }

        // Ensure locked note pk and zk_id attached to this declaration authorized this
        // Operation.
        let note = context
            .locked_notes
            .get(&operation.locked_note_id)
            .expect("The Operation has been checked above");
        if !ZkPublicKey::verify_multi(
            &[note.pk, declaration.zk_id],
            context.tx_hash_view.as_fr(),
            self.proof(),
        ) {
            return Err(SdpError::InvalidZkSignature);
        }

        // Check that the nonce is greater than the previous one
        if operation.nonce <= declaration.nonce {
            return Err(SdpError::InvalidNonce {
                message_nonce: operation.nonce,
                declaration_nonce: declaration.nonce,
            });
        }

        Ok(())
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<SDPWithdrawOp, Verified, Mode>
{
    type Context<'a> = SDPWithdrawExecutionContext;
    type Error = SdpError;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        let declaration = context
            .declarations
            .get_mut(&operation.declaration_id)
            .expect("The operation should have been validated");

        // Delay the withdrawal by `SNAPSHOT_FINALIZATION_DELAY` epochs
        // to prevent "stake-less service provision".
        // Otherwise, providers can continue providing the service even after
        // withdrawal because SDP uses the snapshot from `SNAPSHOT_FINALIZATION_DELAY`
        // epochs ago.
        // The note will be unlocked once the withdrawn epoch set here is reached.
        declaration.withdraw_at = Some(context.epoch.strict_add(sdp::SNAPSHOT_FINALIZATION_DELAY));
        declaration.nonce = operation.nonce;

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
    for SignedOperation<SDPWithdrawOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::ZkKey;
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        mantle::{Note, NoteId, TxHash, ops::op_proof::samples::SampleProof as _},
        sdp::{Declaration, DeclarationMessage, MinStake, ServiceType},
    };

    fn note_key() -> ZkKey {
        ZkKey::from(BigUint::from(1u8))
    }

    fn declaration_key() -> ZkKey {
        ZkKey::from(BigUint::from(2u8))
    }

    fn locked_notes(locked_note_id: &NoteId) -> LockedNotes {
        LockedNotes::new()
            .lock(
                &MinStake {
                    threshold: 0,
                    timestamp: 0,
                },
                ServiceType::BlendNetwork,
                Note::new(10_000, note_key().to_public_key()),
                locked_note_id,
            )
            .expect("the note covers the minimum stake")
    }

    fn declaration(locked_note_id: NoteId) -> Declaration {
        Declaration::new(
            Epoch::from(0),
            &DeclarationMessage {
                zk_id: declaration_key().to_public_key(),
                locked_note_id,
                ..DeclarationMessage::sample()
            },
        )
    }

    fn declarations(operation: &SDPWithdrawOp, declaration: Declaration) -> Declarations {
        Declarations::new_sync().insert(operation.declaration_id, declaration)
    }

    fn preverified(
        operation: SDPWithdrawOp,
        tx_hash_view: &TxHashView,
    ) -> SignedOperation<SDPWithdrawOp, Preverified, StandardMode> {
        let proof = ZkKey::multi_sign(&[note_key(), declaration_key()], tx_hash_view.as_fr())
            .expect("signing should succeed");

        SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
            .into_preverified(&())
            .expect("preverify accepts every withdraw message")
    }

    #[test]
    fn preverify_accepts_every_withdraw_message() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            SDPWithdrawOp::sample(),
            <SDPWithdrawOp as ProvableOperation>::Proof::sample(),
        );

        assert_eq!(signed_operation.preverify(&()), Ok(()));
    }

    #[test]
    fn verify_rejects_an_unknown_declaration() {
        let operation = SDPWithdrawOp::sample();
        let declaration_id = operation.declaration_id;
        let locked_notes = locked_notes(&operation.locked_note_id);
        let declarations = Declarations::new_sync();

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPWithdrawValidationContext {
                declarations: &declarations,
                epoch: Epoch::from(0),
                locked_notes: &locked_notes,
                tx_hash_view: &signed_view,
            }),
            Err(SdpError::DeclarationNotFound(declaration_id))
        );
    }

    #[test]
    fn verify_rejects_a_declaration_already_scheduled_for_withdrawal() {
        let operation = SDPWithdrawOp::sample();
        let declaration_id = operation.declaration_id;
        let locked_notes = locked_notes(&operation.locked_note_id);

        let withdraw_at = Epoch::from(7);
        let declaration = Declaration {
            withdraw_at: Some(withdraw_at),
            ..declaration(operation.locked_note_id)
        };
        let declarations = declarations(&operation, declaration);

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPWithdrawValidationContext {
                declarations: &declarations,
                epoch: Epoch::from(0),
                locked_notes: &locked_notes,
                tx_hash_view: &signed_view,
            }),
            Err(SdpError::DeclarationWithdrawn {
                declaration_id,
                withdraw_at,
            })
        );
    }

    #[test]
    fn verify_rejects_a_note_not_locked_for_the_service() {
        let operation = SDPWithdrawOp::sample();
        let note_id = operation.locked_note_id;
        let locked_notes = LockedNotes::new();
        let declarations = declarations(&operation, declaration(operation.locked_note_id));

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPWithdrawValidationContext {
                declarations: &declarations,
                epoch: Epoch::from(0),
                locked_notes: &locked_notes,
                tx_hash_view: &signed_view,
            }),
            Err(SdpError::NoteNotLockedForService {
                note_id,
                service_type: ServiceType::BlendNetwork,
            })
        );
    }

    #[test]
    fn verify_rejects_a_locked_note_the_declaration_does_not_own() {
        let operation = SDPWithdrawOp::sample();
        let note_id = operation.locked_note_id;
        let expected = NoteId(Fr::from(99u64));
        let locked_notes = locked_notes(&note_id);
        let declarations = declarations(&operation, declaration(expected));

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPWithdrawValidationContext {
                declarations: &declarations,
                epoch: Epoch::from(0),
                locked_notes: &locked_notes,
                tx_hash_view: &signed_view,
            }),
            Err(SdpError::InvalidLockedNote { note_id, expected })
        );
    }

    #[test]
    fn verify_rejects_a_proof_over_another_transaction() {
        let operation = SDPWithdrawOp::sample();
        let locked_notes = locked_notes(&operation.locked_note_id);
        let declarations = declarations(&operation, declaration(operation.locked_note_id));

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let other_view = TxHashView::from(TxHash::from([10u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPWithdrawValidationContext {
                declarations: &declarations,
                epoch: Epoch::from(0),
                locked_notes: &locked_notes,
                tx_hash_view: &signed_view,
            }),
            Ok(())
        );
        assert_eq!(
            signed_operation.verify(&SDPWithdrawValidationContext {
                declarations: &declarations,
                epoch: Epoch::from(0),
                locked_notes: &locked_notes,
                tx_hash_view: &other_view,
            }),
            Err(SdpError::InvalidZkSignature)
        );
    }

    #[test]
    fn verify_rejects_a_proof_missing_the_declaration_key() {
        let operation = SDPWithdrawOp::sample();
        let locked_notes = locked_notes(&operation.locked_note_id);
        let declarations = declarations(&operation, declaration(operation.locked_note_id));

        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            ZkKey::multi_sign(&[note_key()], tx_hash_view.as_fr()).expect("signing should succeed");
        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify accepts every withdraw message");

        assert_eq!(
            signed_operation.verify(&SDPWithdrawValidationContext {
                declarations: &declarations,
                epoch: Epoch::from(0),
                locked_notes: &locked_notes,
                tx_hash_view: &tx_hash_view,
            }),
            Err(SdpError::InvalidZkSignature)
        );
    }

    #[test]
    fn verify_rejects_a_proof_missing_the_locked_note_key() {
        let operation = SDPWithdrawOp::sample();
        let locked_notes = locked_notes(&operation.locked_note_id);
        let declarations = declarations(&operation, declaration(operation.locked_note_id));

        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof = ZkKey::multi_sign(&[declaration_key()], tx_hash_view.as_fr())
            .expect("signing should succeed");
        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify accepts every withdraw message");

        assert_eq!(
            signed_operation.verify(&SDPWithdrawValidationContext {
                declarations: &declarations,
                epoch: Epoch::from(0),
                locked_notes: &locked_notes,
                tx_hash_view: &tx_hash_view,
            }),
            Err(SdpError::InvalidZkSignature)
        );
    }

    #[test]
    fn verify_rejects_a_nonce_that_does_not_increase() {
        let operation = SDPWithdrawOp {
            nonce: 0,
            ..SDPWithdrawOp::sample()
        };
        let locked_notes = locked_notes(&operation.locked_note_id);
        let declarations = declarations(&operation, declaration(operation.locked_note_id));

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPWithdrawValidationContext {
                declarations: &declarations,
                epoch: Epoch::from(0),
                locked_notes: &locked_notes,
                tx_hash_view: &signed_view,
            }),
            Err(SdpError::InvalidNonce {
                message_nonce: 0,
                declaration_nonce: 0,
            })
        );
    }

    fn verified(
        operation: SDPWithdrawOp,
    ) -> SignedOperation<SDPWithdrawOp, Verified, StandardMode> {
        SignedOperation::<_, Unverified, StandardMode>::new(
            operation,
            <SDPWithdrawOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted()
    }

    #[test]
    fn execute_schedules_the_withdrawal_after_the_snapshot_delay() {
        let operation = SDPWithdrawOp::sample();
        let declaration_id = operation.declaration_id;
        let locked_note_id = operation.locked_note_id;
        let nonce = operation.nonce;
        let locked_notes = locked_notes(&locked_note_id);
        let declarations = declarations(&operation, declaration(locked_note_id));
        let epoch = Epoch::from(4);

        let (context, events) = verified(operation)
            .execute(SDPWithdrawExecutionContext {
                declarations,
                locked_notes,
                epoch,
            })
            .expect("the declaration is registered");

        let updated = context
            .declarations
            .get(&declaration_id)
            .expect("the declaration stays registered");
        assert_eq!(
            updated.withdraw_at,
            Some(epoch.strict_add(sdp::SNAPSHOT_FINALIZATION_DELAY))
        );
        assert_eq!(updated.nonce, nonce);
        assert!(
            context
                .locked_notes
                .is_locked_for_service(&locked_note_id, &ServiceType::BlendNetwork)
        );
        assert!(events.is_empty());
    }

    #[test]
    #[should_panic(expected = "The operation should have been validated")]
    fn execute_panics_on_a_declaration_the_ledger_does_not_hold() {
        let operation = SDPWithdrawOp::sample();
        let locked_notes = locked_notes(&operation.locked_note_id);

        drop(verified(operation).execute(SDPWithdrawExecutionContext {
            declarations: Declarations::new_sync(),
            locked_notes,
            epoch: Epoch::from(4),
        }));
    }

    #[test]
    fn sdp_withdraw_op_execution_gas() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            SDPWithdrawOp::sample(),
            <SDPWithdrawOp as ProvableOperation>::Proof::sample(),
        );

        assert_eq!(
            signed_operation.execution_gas::<MainnetGasProfile>(),
            Ok(Gas::new(590))
        );
    }
}
