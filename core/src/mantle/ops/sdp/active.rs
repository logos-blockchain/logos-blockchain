use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use lb_log_targets::mantle;
use tracing::info;

use super::{SDPActiveOp, SdpError};
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
    type Proof = ZkSignature;
    const CODE: u8 = 0x22;
}

impl OperationGas<MainnetGasProfile> for SDPActiveOp {
    const GAS_COST: Gas = Gas::new(590);
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
    type Context<'a> = SDPActiveValidationContext<'a>;
    type Error = SdpError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Check the declaration exists
        let Some(declaration) = context.declarations.get(&operation.declaration_id) else {
            return Err(SdpError::DeclarationNotFound(operation.declaration_id));
        };

        // Check the declaration hasn't been withdrawn
        // (Return error if `scheduled_withdrawal_epoch` epoch has passed)
        if let Some(withdraw_at) = declaration.withdraw_at
            && withdraw_at <= context.epoch
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

        // Check the signature over the `zk_id`
        if !ZkPublicKey::verify_multi(
            &[declaration.zk_id],
            context.tx_hash_view.as_fr(),
            self.proof(),
        ) {
            return Err(SdpError::InvalidZkSignature);
        }

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

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<SDPActiveOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::ZkKey;
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        mantle::{TxHash, ops::op_proof::samples::SampleProof as _},
        sdp::{Declaration, DeclarationMessage},
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
        let proof = ZkKey::multi_sign(&[declaration_key()], tx_hash_view.as_fr())
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
    fn verify_rejects_an_unknown_declaration() {
        let operation = SDPActiveOp::sample();
        let declaration_id = operation.declaration_id;

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPActiveValidationContext {
                declarations: &Declarations::new_sync(),
                tx_hash_view: &signed_view,
                epoch: Epoch::from(0),
            }),
            Err(SdpError::DeclarationNotFound(declaration_id))
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
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPActiveValidationContext {
                declarations: &declarations,
                tx_hash_view: &signed_view,
                epoch: withdraw_at,
            }),
            Err(SdpError::DeclarationWithdrawn {
                declaration_id,
                withdraw_at,
            })
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
            signed_operation.verify(&SDPActiveValidationContext {
                declarations: &declarations,
                tx_hash_view: &signed_view,
                epoch: Epoch::from(0),
            }),
            Err(SdpError::InvalidNonce {
                message_nonce: declaration_nonce,
                declaration_nonce,
            })
        );
    }

    #[test]
    fn verify_rejects_a_proof_over_another_transaction() {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let declarations = Declarations::new_sync().insert(declaration_id, declaration);

        let operation = SDPActiveOp {
            declaration_id,
            ..SDPActiveOp::sample()
        };

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let other_view = TxHashView::from(TxHash::from([10u8; 32]));
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPActiveValidationContext {
                declarations: &declarations,
                tx_hash_view: &signed_view,
                epoch: Epoch::from(0),
            }),
            Ok(())
        );
        assert_eq!(
            signed_operation.verify(&SDPActiveValidationContext {
                declarations: &declarations,
                tx_hash_view: &other_view,
                epoch: Epoch::from(0),
            }),
            Err(SdpError::InvalidZkSignature)
        );
    }

    #[test]
    fn verify_rejects_a_proof_from_a_key_the_declaration_does_not_name() {
        let (message, declaration) = declaration();
        let declaration_id = message.id();
        let declarations = Declarations::new_sync().insert(declaration_id, declaration);

        let operation = SDPActiveOp {
            declaration_id,
            ..SDPActiveOp::sample()
        };

        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof = ZkKey::multi_sign(&[ZkKey::from(BigUint::from(7u8))], tx_hash_view.as_fr())
            .expect("signing should succeed");
        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify accepts every active message");

        assert_eq!(
            signed_operation.verify(&SDPActiveValidationContext {
                declarations: &declarations,
                tx_hash_view: &tx_hash_view,
                epoch: Epoch::from(0),
            }),
            Err(SdpError::InvalidZkSignature)
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
        let signed_operation = preverified(operation, &signed_view);

        assert_eq!(
            signed_operation.verify(&SDPActiveValidationContext {
                declarations: &declarations,
                tx_hash_view: &signed_view,
                epoch: Epoch::from(2),
            }),
            Ok(())
        );
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

    #[test]
    fn sdp_active_op_execution_gas() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            SDPActiveOp::sample(),
            <SDPActiveOp as ProvableOperation>::Proof::sample(),
        );

        assert_eq!(
            signed_operation.execution_gas::<MainnetGasProfile>(),
            Ok(Gas::new(590))
        );
    }
}
