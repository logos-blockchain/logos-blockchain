use lb_binary_codec::canonical::BinaryEncode as _;
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_groth16::Fr;
use lb_log_targets::mantle;
use lb_zktransfer::ZkTransferVerifierInput;
use tracing::debug;

use super::{SDPWithdrawOp, SdpError, validate_declaration_id};
use crate::{
    events::TxEvent,
    mantle::{
        batch::DeferredZkTransfer,
        gas::{
            EXECUTION_SIGNATURE_GAS, Gas, GasOverflow, MainnetGasProfile, OpGasCalculator,
            OpGasContext, OperationGas, spend_gas,
        },
        ledger::{
            Declarations, ExecutableOperation, PreverifiableOperation, ProvableOperation,
            VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        note_set::{CommitmentMmr, NoteSets, SDP_SET},
        notes::InputNullifiers,
        ops::{
            OpId, SignedOperation, ZkAndEd25519Proof,
            channel::withdraw::{PendingWithdrawal, PendingWithdrawals},
        },
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
    sdp::{self, Declaration},
};

const LOG_TARGET: &str = mantle::sdp::message::WITHDRAW;

pub struct SDPWithdrawValidationContext<'a> {
    pub declaration: &'a Declaration,
    pub epoch: Epoch,
    pub note_sets: &'a NoteSets,
    pub tx_hash_view: &'a TxHashView,
}

pub struct SDPWithdrawExecutionContext {
    pub declarations: Declarations,
    pub note_sets: NoteSets,
    pub epoch: Epoch,
    pub pending_withdrawals: PendingWithdrawals,
    // The first slot of epoch `epoch + SNAPSHOT_FINALIZATION_DELAY + 1`, where the
    // declaration is removed and the outputs of the withdrawal are released.
    pub release_slot: Slot,
}

impl ProvableOperation for SDPWithdrawOp {
    type Proof = ZkAndEd25519Proof;
    const CODE: u8 = 0x21;
}

impl OpId for SDPWithdrawOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

impl OperationGas<MainnetGasProfile> for SDPWithdrawOp {
    const GAS_COST: Gas = Gas::new(892);
}

impl OpGasCalculator<MainnetGasProfile> for SDPWithdrawOp {
    // The Operation, plus its signature and the spend of the service note,
    // whose root is of the note alone.
    fn execution_gas(&self, _context: &impl OpGasContext) -> Result<Gas, GasOverflow> {
        Self::GAS_COST
            .checked_add(EXECUTION_SIGNATURE_GAS)?
            .checked_add(spend_gas(1, self.outputs.len())?)
    }
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
    type DeferredProof = DeferredZkTransfer;
    type Context<'a> = SDPWithdrawValidationContext<'a>;
    type Error = SdpError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();

        let declaration = context.declaration;
        validate_declaration_id(operation.declaration_id, declaration)?;

        // Check that the declaration hasn't been already scheduled to be withdrawn.
        if let Some(withdraw_at) = declaration.withdraw_at {
            return Err(SdpError::DeclarationWithdrawn {
                declaration_id: operation.declaration_id,
                withdraw_at,
            });
        }

        // Ensure the `provider_id` attached to this declaration authorized this
        // Operation.
        declaration
            .provider_id
            .0
            .verify(context.tx_hash_view.as_bytes(), &self.proof().ed25519_sig)
            .map_err(|_| SdpError::InvalidEddsaSignature)?;

        // Check that the service note is unspent. The message carries no nonce: it is
        // valid only while no withdrawal is scheduled, and consumes this nullifier
        // once.
        context
            .note_sets
            .get(SDP_SET)
            .expect("the SDP note set always exists")
            .assert_spendable(&[operation.service_note_nullifier])?;

        // The root of an empty MMR to which only the service note of the declaration is
        // appended.
        let tx_commitment_root = CommitmentMmr::new()
            .push(declaration.service_note)
            .expect("one commitment fits in an empty MMR")
            .frontier_root();

        // Defer the proof verification so that the caller can batch it.
        let commitments: Vec<Fr> = operation
            .outputs
            .iter()
            .map(|commitment| commitment.0)
            .collect();
        let public = ZkTransferVerifierInput::new(
            &[operation.service_note_nullifier.0],
            &commitments,
            operation.excess_value,
            tx_commitment_root,
            *context.tx_hash_view.as_fr(),
        )?;
        Ok(DeferredZkTransfer::new(
            *self.proof().zk_proof.as_proof(),
            public,
        ))
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

        // Spend the service note from the SDP set
        let sdp = context
            .note_sets
            .get(SDP_SET)
            .expect("the SDP note set always exists")
            .clone()
            .execute_spending(&[operation.service_note_nullifier])?;
        context.note_sets = context.note_sets.update(SDP_SET, sdp);

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
        // The outputs wait, with no input, until the declaration is removed.
        context.pending_withdrawals = context.pending_withdrawals.push(PendingWithdrawal {
            due: context.release_slot,
            note_set: SDP_SET,
            inputs: InputNullifiers::default(),
            outputs: operation.outputs.clone(),
        });

        debug!(
            target: LOG_TARGET,
            provider_id = ?declaration.provider_id,
            withdraw_at = ?declaration.withdraw_at,
            "updated declaration with withdraw message"
        );

        Ok((context, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, Groth16Input};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey, ZkPublicKey};
    use lb_zktransfer::ZkTransferInputsError;

    use super::*;
    use crate::{
        mantle::{
            TxHash,
            gas::test_utils::FixedThresholds,
            note_set::{LEDGER_SET, NoteSet, NoteSetError},
            notes::{
                self, NoteCommitment, NoteNullifier, OutputCommitments, derive_note_nullifier,
            },
            ops::{
                ZkTransferProof, op_proof::samples::SampleProof as _,
                transfer::test_utils::prove_transfer,
            },
        },
        sdp::DeclarationMessage,
    };

    // The provider of `DeclarationMessage::sample()`.
    fn provider_key() -> Ed25519Key {
        Ed25519Key::from_bytes(&[24; 32])
    }

    fn tx_hash_view() -> TxHashView {
        TxHashView::from(TxHash::from([9u8; 32]))
    }

    fn declaration() -> Declaration {
        Declaration::new(Epoch::from(0), &DeclarationMessage::sample())
    }

    // The sample withdrawal, addressed to the sample declaration.
    fn operation() -> SDPWithdrawOp {
        SDPWithdrawOp {
            declaration_id: DeclarationMessage::sample().id(),
            ..SDPWithdrawOp::sample()
        }
    }

    fn service_note_root(message: &DeclarationMessage) -> Fr {
        CommitmentMmr::new()
            .push(message.service_note().commitment())
            .unwrap()
            .frontier_root()
    }

    fn declarations(operation: &SDPWithdrawOp, declaration: Declaration) -> Declarations {
        Declarations::new_sync().insert(operation.declaration_id, declaration)
    }

    fn preverified(
        operation: SDPWithdrawOp,
        zk_proof: ZkTransferProof,
        signing_key: &Ed25519Key,
        signed_view: &TxHashView,
    ) -> SignedOperation<SDPWithdrawOp, Preverified, StandardMode> {
        SignedOperation::<_, Unverified, StandardMode>::new(
            operation,
            ZkAndEd25519Proof::new(
                zk_proof,
                signing_key.sign_payload(signed_view.as_bytes().as_ref()),
            ),
        )
        .into_preverified(&())
        .expect("preverify accepts every withdraw message")
    }

    fn verify_withdrawal(
        signed_operation: &SignedOperation<SDPWithdrawOp, Preverified, StandardMode>,
        declaration: &Declaration,
        note_sets: &NoteSets,
    ) -> Result<DeferredZkTransfer, SdpError> {
        signed_operation.verify(&SDPWithdrawValidationContext {
            declaration,
            epoch: Epoch::from(0),
            note_sets,
            tx_hash_view: &tx_hash_view(),
        })
    }

    fn verify_sample(
        operation: SDPWithdrawOp,
        declaration: &Declaration,
    ) -> Result<DeferredZkTransfer, SdpError> {
        verify_withdrawal(
            &preverified(
                operation,
                ZkTransferProof::sample(),
                &provider_key(),
                &tx_hash_view(),
            ),
            declaration,
            &NoteSets::new(),
        )
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
    fn verify_rejects_a_declaration_already_scheduled_for_withdrawal() {
        let operation = operation();
        let declaration_id = operation.declaration_id;

        let withdraw_at = Epoch::from(7);
        let declaration = Declaration {
            withdraw_at: Some(withdraw_at),
            ..declaration()
        };

        assert_eq!(
            verify_sample(operation, &declaration).unwrap_err(),
            SdpError::DeclarationWithdrawn {
                declaration_id,
                withdraw_at,
            }
        );
    }

    #[test]
    fn verify_rejects_a_context_declaration_with_a_different_id() {
        let operation = SDPWithdrawOp::sample();
        let operation_declaration_id = operation.declaration_id;
        let declaration = declaration();
        let supplied_declaration_id = declaration.id();

        assert_eq!(
            verify_sample(operation, &declaration).unwrap_err(),
            SdpError::DeclarationIdMismatch {
                operation_declaration_id,
                supplied_declaration_id,
            }
        );
    }

    #[test]
    fn verify_defers_the_zk_transfer_against_the_service_note_root() {
        let declaration = declaration();
        let operation = operation();
        // Other notes of the SDP set do not change the root the withdrawal is proven
        // against.
        let sdp = NoteSet::empty()
            .execute_adding(&[NoteCommitment(Fr::from(1u64))])
            .unwrap();
        let sets = NoteSets::new().update(SDP_SET, sdp).end_block(0);
        let deferred = verify_withdrawal(
            &preverified(
                operation,
                ZkTransferProof::sample(),
                &provider_key(),
                &tx_hash_view(),
            ),
            &declaration,
            &sets,
        )
        .unwrap();
        let public = deferred.inputs();
        assert_eq!(
            public.inputs.map(Groth16Input::into_inner),
            [Fr::from(28u64), Fr::ZERO, Fr::ZERO, Fr::ZERO]
        );
        assert_eq!(public.outputs[0].into_inner(), Fr::from(46u64));
        assert_eq!(public.outputs[1].into_inner(), Fr::ZERO);
        assert_eq!(public.excess_value.into_inner(), Fr::from(47u64));
        assert_eq!(
            public.tx_commitment_root.into_inner(),
            service_note_root(&DeclarationMessage::sample())
        );
        assert_eq!(public.msg.into_inner(), *tx_hash_view().as_fr());
    }

    #[test]
    fn verify_rejects_a_signature_by_another_key() {
        assert_eq!(
            verify_withdrawal(
                &preverified(
                    operation(),
                    ZkTransferProof::sample(),
                    &Ed25519Key::from_bytes(&[25; 32]),
                    &tx_hash_view()
                ),
                &declaration(),
                &NoteSets::new(),
            )
            .unwrap_err(),
            SdpError::InvalidEddsaSignature
        );
    }

    #[test]
    fn verify_rejects_a_spent_service_note() {
        let spent = NoteSet::empty()
            .execute_spending(&[NoteNullifier(Fr::from(28u64))])
            .unwrap();
        let sets = NoteSets::new().update(SDP_SET, spent);
        assert_eq!(
            verify_withdrawal(
                &preverified(
                    operation(),
                    ZkTransferProof::sample(),
                    &provider_key(),
                    &tx_hash_view()
                ),
                &declaration(),
                &sets,
            )
            .unwrap_err(),
            SdpError::NoteSet(NoteSetError::AlreadySpent(NoteNullifier(Fr::from(28u64))))
        );
    }

    #[test]
    fn verify_rejects_more_than_eight_outputs() {
        let operation = SDPWithdrawOp {
            outputs: OutputCommitments::try_from_iter(
                (1..=9u64).map(|n| NoteCommitment(Fr::from(n))),
            )
            .unwrap(),
            ..operation()
        };
        assert_eq!(
            verify_sample(operation, &declaration()).unwrap_err(),
            SdpError::ZkTransferInputs(ZkTransferInputsError::TooManyOutputs(9))
        );
    }

    #[test]
    fn verify_builds_the_public_inputs_a_real_proof_satisfies() {
        let secret_key = Fr::from(3u64);
        let zk_id_secret_key = Fr::from(5u64);
        let message = DeclarationMessage {
            zk_id: ZkKey::new(zk_id_secret_key).to_public_key(),
            note_pk: ZkKey::new(secret_key).to_public_key(),
            ..DeclarationMessage::sample()
        };
        let service_note = message.service_note();
        assert_eq!(service_note.public_key, message.note_pk);
        assert_ne!(service_note.public_key, message.zk_id);
        let (mmr, path) = CommitmentMmr::new()
            .push_with_paths(service_note.commitment(), &mut [])
            .unwrap();
        let output = notes::Note::new(40, Fr::from(12u64), ZkPublicKey::new(Fr::from(2u64)));
        let withdrawal = |excess_value| SDPWithdrawOp {
            declaration_id: message.id(),
            service_note_nullifier: derive_note_nullifier(
                &service_note.commitment(),
                &ZkKey::new(secret_key).nullifier_key(),
            ),
            outputs: [output.commitment()].into(),
            excess_value,
        };
        let declaration = Declaration::new(Epoch::from(0), &message);
        let proof = prove_transfer(
            &[(service_note, secret_key, path)],
            &[output],
            mmr.frontier_root(),
            *tx_hash_view().as_fr(),
        );

        let deferred = verify_withdrawal(
            &preverified(
                withdrawal(5),
                ZkTransferProof::new(proof),
                &provider_key(),
                &tx_hash_view(),
            ),
            &declaration,
            &NoteSets::new(),
        )
        .unwrap();
        assert!(lb_zktransfer::verify(deferred.proof(), deferred.inputs()).unwrap());

        let deferred = verify_withdrawal(
            &preverified(
                withdrawal(6),
                ZkTransferProof::new(proof),
                &provider_key(),
                &tx_hash_view(),
            ),
            &declaration,
            &NoteSets::new(),
        )
        .unwrap();
        assert!(!lb_zktransfer::verify(deferred.proof(), deferred.inputs()).unwrap());

        // The zk_id secret only opens a note under the zk_id, whose commitment
        // is not the one the declaration holds.
        let zk_id_note = notes::Note::new(service_note.value, service_note.nonce, message.zk_id);
        let (zk_id_mmr, zk_id_path) = CommitmentMmr::new()
            .push_with_paths(zk_id_note.commitment(), &mut [])
            .unwrap();
        let zk_id_proof = prove_transfer(
            &[(zk_id_note, zk_id_secret_key, zk_id_path)],
            &[output],
            zk_id_mmr.frontier_root(),
            *tx_hash_view().as_fr(),
        );
        let deferred = verify_withdrawal(
            &preverified(
                SDPWithdrawOp {
                    service_note_nullifier: derive_note_nullifier(
                        &zk_id_note.commitment(),
                        &ZkKey::new(zk_id_secret_key).nullifier_key(),
                    ),
                    ..withdrawal(5)
                },
                ZkTransferProof::new(zk_id_proof),
                &provider_key(),
                &tx_hash_view(),
            ),
            &declaration,
            &NoteSets::new(),
        )
        .unwrap();
        assert!(!lb_zktransfer::verify(deferred.proof(), deferred.inputs()).unwrap());
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
    fn execute_schedules_the_withdrawal_and_spends_the_service_note() {
        let operation = operation();
        let declaration_id = operation.declaration_id;
        let declarations = declarations(&operation, declaration());
        let (context, events) = verified(operation)
            .execute(SDPWithdrawExecutionContext {
                declarations,
                note_sets: NoteSets::new(),
                epoch: Epoch::from(4),
                pending_withdrawals: PendingWithdrawals::new(),
                release_slot: Slot::from(70u64),
            })
            .expect("the declaration is registered");

        let updated = context.declarations.get(&declaration_id).unwrap();
        assert_eq!(
            updated.withdraw_at,
            Some(Epoch::from(4).strict_add(sdp::SNAPSHOT_FINALIZATION_DELAY))
        );
        // The outputs wait, with no input, for the removal of the declaration.
        assert_eq!(
            context.pending_withdrawals.iter().collect::<Vec<_>>(),
            vec![&PendingWithdrawal {
                due: Slot::from(70u64),
                note_set: SDP_SET,
                inputs: InputNullifiers::default(),
                outputs: [NoteCommitment(Fr::from(46u64))].into(),
            }]
        );
        assert!(
            context
                .note_sets
                .get(SDP_SET)
                .unwrap()
                .contains_nullifier(&NoteNullifier(Fr::from(28u64)))
        );
        assert_eq!(
            context.note_sets.get(LEDGER_SET).unwrap().commitment_root(),
            NoteSet::empty().commitment_root()
        );
        assert_eq!(events, []);
    }

    #[test]
    #[should_panic(expected = "The operation should have been validated")]
    fn execute_panics_on_a_declaration_the_ledger_does_not_hold() {
        let operation = SDPWithdrawOp::sample();

        drop(verified(operation).execute(SDPWithdrawExecutionContext {
            declarations: Declarations::new_sync(),
            note_sets: NoteSets::new(),
            epoch: Epoch::from(4),
            pending_withdrawals: PendingWithdrawals::new(),
            release_slot: Slot::from(70u64),
        }));
    }

    #[test]
    fn sdp_withdraw_op_execution_gas_does_not_scale_with_the_threshold() {
        // The service note, 1 output, no buffer: 892 + 161 + 1770 + 725 + 24
        for threshold in [0, 1, 3] {
            assert_eq!(
                SDPWithdrawOp::sample().execution_gas(&FixedThresholds(threshold)),
                Ok(Gas::new(3_572))
            );
        }
    }
}
