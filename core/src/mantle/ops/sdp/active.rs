use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{ZkSignature, public_inputs_from_pks};
use lb_log_targets::mantle;
use tracing::info;

use super::{SDPActiveOp, SdpError, validate_declaration_id, validate_nonce};
use crate::{
    events::TxEvent,
    mantle::{
        batch::DeferredZkpVerification,
        gas::{Gas, MainnetGasProfile, OpGasCalculator, OperationGas},
        ledger::{
            Declarations, ExecutableOperation, PreverifiableOperation, ProvableOperation,
            VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::SignedOperation,
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
    sdp::Declaration,
};

const LOG_TARGET: &str = mantle::sdp::message::ACTIVE;

pub struct SDPActiveValidationContext<'a> {
    pub declaration: &'a Declaration,
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

impl OpGasCalculator<MainnetGasProfile> for SDPActiveOp {}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<SDPActiveOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = SdpError;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl VerifiableOperation<StandardMode> for SignedOperation<SDPActiveOp, Preverified, StandardMode> {
    type Context<'a> = SDPActiveValidationContext<'a>;
    type Error = SdpError;

    fn verify(
        &self,
        context: &Self::Context<'_>,
    ) -> Result<Option<DeferredZkpVerification>, Self::Error> {
        let operation = self.operation();

        let declaration = context.declaration;
        validate_declaration_id(operation.declaration_id, declaration)?;

        // Check the declaration hasn't been withdrawn.
        // The report attesting `withdraw_at - 1` is due during `withdraw_at`,
        // so the message is valid while the current epoch is at most `withdraw_at`.
        if let Some(withdraw_at) = declaration.withdraw_at
            && withdraw_at < context.epoch
        {
            return Err(SdpError::DeclarationWithdrawn {
                declaration_id: operation.declaration_id,
                withdraw_at,
            });
        }

        validate_nonce(operation.nonce, declaration)?;

        // Defer the proof verification, so that the caller can batch it.
        let inputs =
            public_inputs_from_pks((*context.tx_hash_view.as_fr()).into(), &[declaration.zk_id])
                .map_err(|_| SdpError::InvalidZkSignature)?;
        Ok(Some(DeferredZkpVerification::ZkSig(
            *self.proof().as_proof(),
            inputs,
        )))
    }
}

impl<Mode: VerificationMode> ExecutableOperation for SignedOperation<SDPActiveOp, Verified, Mode> {
    type Context<'a> = SDPActiveExecutionContext;
    type Error = SdpError;

    // TODO: check service specific logic
    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        let declaration = context
            .declarations
            .get_mut(&operation.declaration_id)
            .expect("The operation should have been validated");

        declaration.active = context.epoch;
        declaration.nonce = operation.nonce;
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

#[cfg(test)]
mod tests {
    use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, CompressedGroth16Proof, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey, ZkSignature};
    use num_bigint::BigUint;

    use super::{SDPActiveOp, SDPActiveValidationContext, SdpError};
    use crate::{
        mantle::{
            TxHash,
            batch::{DeferredZkpVerification, Error as BatchError, test_utils::batch_verify},
            gas::{Gas, OpGasCalculator as _, test_utils::FixedThresholds},
            ledger::{
                Declarations, PreverifiableOperation as _, ProvableOperation,
                VerifiableOperation as _, verification_mode::StandardMode,
            },
            ops::{
                SignedOperation,
                op_proof::samples::SampleProof as _,
                sdp::{SDPActiveExecutionContext, SDPDeclareOp},
            },
            transactions::{
                hash::TxHashView,
                states::{Preverified, Unverified, Verified},
            },
        },
        sdp::{
            ActivityMetadata, Declaration, DeclarationMessage, Nonce, ServiceType,
            blend::ActivityProof,
        },
    };

    fn declaration_key() -> ZkKey {
        ZkKey::from(BigUint::from(1u8))
    }

    fn declaration() -> (DeclarationMessage, Declaration) {
        let message = DeclarationMessage {
            zk_id: declaration_key().to_public_key(),
            ..DeclarationMessage::sample()
        };
        let declaration = Declaration::new(Epoch::from(0), &message);

        (message, declaration)
    }

    fn preverified(
        operation: SDPActiveOp,
        tx_hash_view: &TxHashView,
    ) -> SignedOperation<SDPActiveOp, Preverified, StandardMode> {
        preverified_with_key(operation, tx_hash_view, declaration_key())
    }

    fn preverified_with_key(
        operation: SDPActiveOp,
        tx_hash_view: &TxHashView,
        signing_key: ZkKey,
    ) -> SignedOperation<SDPActiveOp, Preverified, StandardMode> {
        let proof = ZkKey::multi_sign(&[signing_key], tx_hash_view.as_fr())
            .expect("signing should succeed");

        SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
            .into_preverified(&())
            .expect("preverify accepts every active message")
    }

    #[test]
    fn preverify_accepts_every_active_message() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            SDPActiveOp::sample(),
            <SDPActiveOp as ProvableOperation>::Proof::sample(),
        );

        assert_eq!(signed_operation.preverify(&()), Ok(()));
    }

    #[test]
    fn verify_rejects_a_declaration_whose_withdrawal_epoch_has_passed() {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let withdraw_at = Epoch::from(3);
        let declarations = Declarations::new_sync().insert(
            declaration_id,
            Declaration {
                withdraw_at: Some(withdraw_at),
                ..declaration
            },
        );

        let operation = SDPActiveOp {
            declaration_id,
            ..SDPActiveOp::sample()
        };

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation
                .verify(&SDPActiveValidationContext {
                    declaration: declarations.get(&declaration_id).expect("declaration"),
                    tx_hash_view: &signed_view,
                    epoch: withdraw_at.strict_add(Epoch::from(1)),
                })
                .unwrap_err(),
            SdpError::DeclarationWithdrawn {
                declaration_id,
                withdraw_at,
            }
        );
    }

    #[test]
    fn verify_rejects_a_context_declaration_with_a_different_id() {
        let (message_a, _) = declaration();
        let declaration_id_a = message_a.id();
        let declaration_key_b = ZkKey::from(BigUint::from(7u8));
        let message_b = DeclarationMessage {
            zk_id: declaration_key_b.to_public_key(),
            ..message_a
        };
        let declaration_b = Declaration::new(Epoch::from(0), &message_b);
        let declaration_id_b = declaration_b.id();
        assert_ne!(declaration_id_a, declaration_id_b);

        let operation = SDPActiveOp {
            declaration_id: declaration_id_a,
            nonce: Nonce::new(declaration_b.created, 1),
            ..SDPActiveOp::sample()
        };
        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified_with_key(operation, &tx_hash_view, declaration_key_b);

        assert_eq!(
            signed_operation
                .verify(&SDPActiveValidationContext {
                    declaration: &declaration_b,
                    tx_hash_view: &tx_hash_view,
                    epoch: declaration_b.created,
                })
                .unwrap_err(),
            SdpError::DeclarationIdMismatch {
                operation_declaration_id: declaration_id_a,
                supplied_declaration_id: declaration_id_b,
            }
        );
    }

    #[test]
    fn verify_rejects_a_nonce_that_does_not_increase() {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let declaration_nonce = declaration.nonce;
        let declarations = Declarations::new_sync().insert(declaration_id, declaration);

        let operation = SDPActiveOp {
            declaration_id,
            nonce: declaration_nonce,
            ..SDPActiveOp::sample()
        };

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation
                .verify(&SDPActiveValidationContext {
                    declaration: declarations.get(&declaration_id).expect("declaration"),
                    tx_hash_view: &signed_view,
                    epoch: Epoch::from(0),
                })
                .unwrap_err(),
            SdpError::InvalidNonceSequence {
                nonce_sequence: declaration_nonce.sequence(),
                declaration_sequence: declaration_nonce.sequence(),
            }
        );
    }

    #[test]
    fn verify_rejects_a_nonce_from_another_declaration_lifecycle() {
        let (message, declaration_a) = declaration();
        let declaration_id = message.id();
        let declaration_b = Declaration::new(Epoch::new(3), &message);
        let declarations = Declarations::new_sync().insert(declaration_id, declaration_b.clone());
        let operation = SDPActiveOp {
            declaration_id,
            nonce: Nonce::new(declaration_a.created, 1),
            ..SDPActiveOp::sample()
        };
        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));

        assert_eq!(
            preverified(operation, &signed_view)
                .verify(&SDPActiveValidationContext {
                    declaration: declarations.get(&declaration_id).expect("declaration"),
                    tx_hash_view: &signed_view,
                    epoch: declaration_b.created,
                })
                .unwrap_err(),
            SdpError::InvalidNonceLifecycle {
                nonce_lifecycle_epoch: declaration_a.created,
                declaration_created_epoch: declaration_b.created,
            }
        );
    }

    #[test]
    fn verify_accepts_a_sequence_jump_within_the_same_lifecycle() {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let declarations = Declarations::new_sync().insert(declaration_id, declaration.clone());
        let operation = SDPActiveOp {
            declaration_id,
            nonce: Nonce::new(declaration.created, 100),
            ..SDPActiveOp::sample()
        };
        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));

        assert!(
            preverified(operation, &signed_view)
                .verify(&SDPActiveValidationContext {
                    declaration: declarations.get(&declaration_id).expect("declaration"),
                    tx_hash_view: &signed_view,
                    epoch: declaration.created,
                })
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn verify_rejects_a_lower_sequence_in_the_same_lifecycle() {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let declaration = Declaration {
            nonce: Nonce::new(declaration.created, 4),
            ..declaration
        };
        let declarations = Declarations::new_sync().insert(declaration_id, declaration.clone());
        let operation = SDPActiveOp {
            declaration_id,
            nonce: Nonce::new(declaration.created, 3),
            ..SDPActiveOp::sample()
        };
        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));

        assert_eq!(
            preverified(operation, &signed_view)
                .verify(&SDPActiveValidationContext {
                    declaration: declarations.get(&declaration_id).expect("declaration"),
                    tx_hash_view: &signed_view,
                    epoch: declaration.created,
                })
                .unwrap_err(),
            SdpError::InvalidNonceSequence {
                nonce_sequence: 3,
                declaration_sequence: 4,
            }
        );
    }

    fn unrelated_key() -> ZkKey {
        ZkKey::from(BigUint::from(7u8))
    }

    fn deferred_zkp_signed_by(signers: &[ZkKey]) -> Option<DeferredZkpVerification> {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let declarations = Declarations::new_sync().insert(declaration_id, declaration);
        let operation = SDPActiveOp {
            declaration_id,
            ..SDPActiveOp::sample()
        };
        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            ZkKey::multi_sign(signers, tx_hash_view.as_fr()).expect("signing should succeed");

        SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
            .into_preverified(&())
            .expect("preverify accepts every active message")
            .verify(&SDPActiveValidationContext {
                declaration: declarations.get(&declaration_id).expect("declaration"),
                tx_hash_view: &tx_hash_view,
                epoch: Epoch::from(0),
            })
            .expect("verify leaves the proof to the batch")
    }

    #[test]
    fn deferred_zkp_is_accepted() {
        assert!(batch_verify(deferred_zkp_signed_by(&[declaration_key()])).is_ok());
    }

    #[test]
    fn wrong_deferred_zkp_is_rejected() {
        assert!(matches!(
            batch_verify(deferred_zkp_signed_by(&[unrelated_key()])),
            Err(BatchError::InvalidZkSignatures)
        ));
    }

    #[test]
    fn verify_accepts_a_declaration_whose_withdrawal_epoch_is_still_ahead() {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let declarations = Declarations::new_sync().insert(
            declaration_id,
            Declaration {
                withdraw_at: Some(Epoch::from(3)),
                ..declaration
            },
        );

        let operation = SDPActiveOp {
            declaration_id,
            ..SDPActiveOp::sample()
        };

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert!(
            signed_operation
                .verify(&SDPActiveValidationContext {
                    declaration: declarations.get(&declaration_id).expect("declaration"),
                    tx_hash_view: &signed_view,
                    epoch: Epoch::from(2),
                })
                .unwrap()
                .is_some()
        );
    }

    const WITHDRAW_AT: Epoch = Epoch::new(5);

    /// The report attesting `withdraw_at - 1` is due during `withdraw_at`,
    /// so an active message included at `withdraw_at` must be accepted.
    #[test]
    fn accepts_active_message_at_withdraw_at() {
        verify_active_at(WITHDRAW_AT).unwrap();
    }

    #[test]
    fn rejects_active_message_after_withdraw_at() {
        assert!(matches!(
            verify_active_at(WITHDRAW_AT.strict_add(Epoch::new(1))),
            Err(SdpError::DeclarationWithdrawn { .. })
        ));
    }

    fn verified(operation: SDPActiveOp) -> SignedOperation<SDPActiveOp, Verified, StandardMode> {
        SignedOperation::<_, Unverified, StandardMode>::new(
            operation,
            <SDPActiveOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted()
    }

    #[test]
    #[should_panic(expected = "The operation should have been validated")]
    fn execute_panics_on_a_declaration_the_ledger_does_not_hold() {
        drop(
            verified(SDPActiveOp::sample()).execute(SDPActiveExecutionContext {
                epoch: Epoch::from(4),
                declarations: Declarations::new_sync(),
            }),
        );
    }

    #[test]
    fn execute_marks_the_declaration_active_for_the_current_epoch() {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let declarations = Declarations::new_sync().insert(declaration_id, declaration);

        let operation = SDPActiveOp {
            declaration_id,
            ..SDPActiveOp::sample()
        };
        let nonce = operation.nonce;
        let epoch = Epoch::from(4);

        let (context, events) = verified(operation)
            .execute(SDPActiveExecutionContext {
                epoch,
                declarations,
            })
            .expect("the declaration is registered");

        let updated = context
            .declarations
            .get(&declaration_id)
            .expect("the declaration stays registered");
        assert_eq!(updated.active, epoch);
        assert_eq!(updated.nonce, nonce);
        assert!(events.is_empty());
    }

    /// Verifies an active message at `epoch` against a declaration whose
    /// `withdraw_at` is [`WITHDRAW_AT`].
    fn verify_active_at(epoch: Epoch) -> Result<(), SdpError> {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let declare_op = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: vec!["/ip4/1.1.1.1/udp/0".parse().unwrap()]
                .try_into()
                .unwrap(),
            provider_id: signing_key.public_key().into(),
            zk_id: ZkKey::from(BigUint::from(1u64)).to_public_key(),
            service_note_id: Fr::ZERO.into(),
        };
        let mut declaration = Declaration::new(Epoch::new(0), &declare_op);
        declaration.withdraw_at = Some(WITHDRAW_AT);
        let declarations = Declarations::new_sync().insert(declare_op.id(), declaration);

        let active_op = SDPActiveOp {
            declaration_id: declare_op.id(),
            nonce: Nonce::new(Epoch::new(0), 1),
            metadata: ActivityMetadata::Blend(Box::new(ActivityProof {
                epoch: Epoch::new(0),
                signing_key: signing_key.public_key(),
                proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]).into(),
                proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]).into(),
            })),
        };
        let signed_operation: SignedOperation<SDPActiveOp, Preverified, StandardMode> =
            SignedOperation::new(
                active_op,
                ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            )
            .into_state_trusted();

        let tx_hash_view = TxHashView::from(TxHash::default());
        signed_operation
            .verify(&SDPActiveValidationContext {
                declaration: declarations.get(&declare_op.id()).expect("declaration"),
                tx_hash_view: &tx_hash_view,
                epoch,
            })
            .map(|_| ())
    }

    #[test]
    fn sdp_active_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            assert_eq!(
                SDPActiveOp::sample().execution_gas(&FixedThresholds(threshold)),
                Ok(Gas::new(590))
            );
        }
    }
}
