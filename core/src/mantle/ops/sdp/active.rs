use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_log_targets::mantle;
use tracing::info;

use super::{SDPActiveOp, SdpError};
use crate::{
    events::TxEvent,
    mantle::{
        gas::{
            EXECUTION_SIGNATURE_GAS, Gas, GasOverflow, MainnetGasProfile, OpGasCalculator,
            OpGasContext, OperationGas,
        },
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
    type Proof = Ed25519Signature;
    const CODE: u8 = 0x22;
}

impl OperationGas<MainnetGasProfile> for SDPActiveOp {
    const GAS_COST: Gas = Gas::new(6975);
}

impl OpGasCalculator<MainnetGasProfile> for SDPActiveOp {
    // The Operation, plus its signature.
    fn execution_gas(&self, _context: &impl OpGasContext) -> Result<Gas, GasOverflow> {
        Self::GAS_COST.checked_add(EXECUTION_SIGNATURE_GAS)
    }
}

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
    type DeferredProof = ();
    type Context<'a> = SDPActiveValidationContext<'a>;
    type Error = SdpError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();

        // Check the declaration exists
        let Some(declaration) = context.declarations.get(&operation.declaration_id) else {
            return Err(SdpError::DeclarationNotFound(operation.declaration_id));
        };

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

        // Check the nonce is increasing
        if operation.nonce <= declaration.nonce {
            return Err(SdpError::InvalidNonce {
                message_nonce: operation.nonce,
                declaration_nonce: declaration.nonce,
            });
        }

        // Ensure the `provider_id` attached to this declaration authorized this
        // Operation.
        declaration
            .provider_id
            .0
            .verify(context.tx_hash_view.as_bytes(), self.proof())
            .map_err(|_| SdpError::InvalidEddsaSignature)?;
        Ok(())
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
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;

    use super::{SDPActiveOp, SDPActiveValidationContext, SdpError};
    use crate::{
        mantle::{
            TxHash,
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
        sdp::{ActivityMetadata, Declaration, DeclarationMessage, blend::ActivityProof},
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

    // The provider of `DeclarationMessage::sample()`.
    fn provider_key() -> Ed25519Key {
        Ed25519Key::from_bytes(&[24; 32])
    }

    fn preverified(
        operation: SDPActiveOp,
        signing_key: &Ed25519Key,
        tx_hash_view: &TxHashView,
    ) -> SignedOperation<SDPActiveOp, Preverified, StandardMode> {
        SignedOperation::<_, Unverified, StandardMode>::new(
            operation,
            signing_key.sign_payload(tx_hash_view.as_bytes().as_ref()),
        )
        .into_preverified(&())
        .expect("preverify accepts every active message")
    }

    fn registered_active_op() -> (SDPActiveOp, Declarations) {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let operation = SDPActiveOp {
            declaration_id,
            ..SDPActiveOp::sample()
        };
        (
            operation,
            Declarations::new_sync().insert(declaration_id, declaration),
        )
    }

    #[test]
    fn verify_accepts_the_provider_signature() {
        let (operation, declarations) = registered_active_op();
        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        assert_eq!(
            preverified(operation, &provider_key(), &signed_view).verify(
                &SDPActiveValidationContext {
                    declarations: &declarations,
                    tx_hash_view: &signed_view,
                    epoch: Epoch::from(0),
                }
            ),
            Ok(())
        );
    }

    #[test]
    fn verify_rejects_a_signature_by_another_key() {
        let (operation, declarations) = registered_active_op();
        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        assert_eq!(
            preverified(operation, &Ed25519Key::from_bytes(&[25; 32]), &signed_view).verify(
                &SDPActiveValidationContext {
                    declarations: &declarations,
                    tx_hash_view: &signed_view,
                    epoch: Epoch::from(0),
                }
            ),
            Err(SdpError::InvalidEddsaSignature)
        );
    }

    #[test]
    fn verify_rejects_a_signature_over_another_transaction() {
        let (operation, declarations) = registered_active_op();
        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let other_view = TxHashView::from(TxHash::from([10u8; 32]));
        assert_eq!(
            preverified(operation, &provider_key(), &other_view).verify(
                &SDPActiveValidationContext {
                    declarations: &declarations,
                    tx_hash_view: &signed_view,
                    epoch: Epoch::from(0),
                }
            ),
            Err(SdpError::InvalidEddsaSignature)
        );
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
    fn verify_rejects_an_unknown_declaration() {
        let operation = SDPActiveOp::sample();
        let declaration_id = operation.declaration_id;

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &provider_key(), &signed_view);

        assert_eq!(
            signed_operation
                .verify(&SDPActiveValidationContext {
                    declarations: &Declarations::new_sync(),
                    tx_hash_view: &signed_view,
                    epoch: Epoch::from(0),
                })
                .unwrap_err(),
            SdpError::DeclarationNotFound(declaration_id)
        );
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
        let signed_operation = preverified(operation, &provider_key(), &signed_view);

        assert_eq!(
            signed_operation
                .verify(&SDPActiveValidationContext {
                    declarations: &declarations,
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
        let signed_operation = preverified(operation, &provider_key(), &signed_view);

        assert_eq!(
            signed_operation
                .verify(&SDPActiveValidationContext {
                    declarations: &declarations,
                    tx_hash_view: &signed_view,
                    epoch: Epoch::from(0),
                })
                .unwrap_err(),
            SdpError::InvalidNonce {
                message_nonce: declaration_nonce,
                declaration_nonce,
            }
        );
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
        let signed_operation = preverified(operation, &provider_key(), &signed_view);

        signed_operation
            .verify(&SDPActiveValidationContext {
                declarations: &declarations,
                tx_hash_view: &signed_view,
                epoch: Epoch::from(2),
            })
            .expect("a withdrawal epoch still ahead keeps the declaration active");
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
        assert_eq!(events, []);
    }

    /// Verifies an active message at `epoch` against a declaration whose
    /// `withdraw_at` is [`WITHDRAW_AT`].
    fn verify_active_at(epoch: Epoch) -> Result<(), SdpError> {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let declare_op = SDPDeclareOp {
            locators: vec!["/ip4/1.1.1.1/udp/3000".parse().unwrap()]
                .try_into()
                .unwrap(),
            provider_id: signing_key.public_key().into(),
            zk_id: ZkKey::from(BigUint::from(1u64)).to_public_key(),
            ..SDPDeclareOp::sample()
        };
        let mut declaration = Declaration::new(Epoch::new(0), &declare_op);
        declaration.withdraw_at = Some(WITHDRAW_AT);
        let declarations = Declarations::new_sync().insert(declare_op.id(), declaration);

        let active_op = SDPActiveOp {
            declaration_id: declare_op.id(),
            nonce: 1,
            metadata: ActivityMetadata::Blend(Box::new(ActivityProof {
                epoch: Epoch::new(0),
                signing_key: signing_key.public_key(),
                proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]).into(),
                proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]).into(),
            })),
        };
        let tx_hash_view = TxHashView::from(TxHash::default());
        let signed_operation: SignedOperation<SDPActiveOp, Preverified, StandardMode> =
            SignedOperation::new(
                active_op,
                signing_key.sign_payload(tx_hash_view.as_bytes().as_ref()),
            )
            .into_state_trusted();

        signed_operation.verify(&SDPActiveValidationContext {
            declarations: &declarations,
            tx_hash_view: &tx_hash_view,
            epoch,
        })
    }

    #[test]
    fn sdp_active_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            assert_eq!(
                SDPActiveOp::sample().execution_gas(&FixedThresholds(threshold)),
                // 6975 + 167
                Ok(Gas::new(7_142))
            );
        }
    }
}
