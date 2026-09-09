use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::{ZkSignature, public_inputs_from_pks};
use lb_log_targets::mantle;
use tracing::info;

use super::{SDPActiveOp, SdpError};
use crate::{
    events::TxEvent,
    mantle::{
        Value,
        batch::DeferredZkpVerification,
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

    fn verify(
        &self,
        context: &Self::Context<'_>,
    ) -> Result<Option<DeferredZkpVerification>, Self::Error> {
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

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<SDPActiveOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
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
            ledger::{Declarations, VerifiableOperation as _, verification_mode::StandardMode},
            ops::{SignedOperation, sdp::SDPDeclareOp},
            transactions::{
                hash::{TxHash, TxHashView},
                states::Preverified,
            },
        },
        sdp::{ActivityMetadata, Declaration, ServiceType, blend::ActivityProof},
    };

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
            nonce: 1,
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
                declarations: &declarations,
                tx_hash_view: &tx_hash_view,
                epoch,
            })
            .map(|_| ())
    }
}
