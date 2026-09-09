use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_key_management_system_keys::keys::{ZkSignature, public_inputs_from_pks};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    events::TxEvent,
    mantle::{
        Value,
        batch::DeferredZkpVerification,
        channel::Channels,
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            self, ExecutableOperation, Inputs, Outputs, PreverifiableOperation, ProvableOperation,
            Utxo, Utxos, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::{OpId, SignedOperation},
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, VerificationState, Verified},
        },
    },
    sdp::service_notes::ServiceNotes,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, BinaryCodec)]
pub struct TransferOp {
    pub inputs: Inputs,
    pub outputs: Outputs,
}

impl TransferOp {
    #[must_use]
    pub const fn new(inputs: Inputs, outputs: Outputs) -> Self {
        Self { inputs, outputs }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.outputs.is_empty()
    }

    pub fn utxos(&self) -> impl Iterator<Item = Utxo> {
        self.outputs.utxos(self)
    }

    #[must_use]
    pub fn utxo_by_index(&self, index: usize) -> Option<Utxo> {
        self.outputs.utxo_by_index(index, self)
    }

    pub fn balance(&self, utxos: &Utxos) -> Result<i128, TransferError> {
        let mut balance: i128 = 0;
        let input_amount = self.inputs.amount(utxos)?;
        let output_amount = self.outputs.amount()?;
        balance = balance
            .checked_add(i128::from(input_amount))
            .ok_or(TransferError::BalanceOverflow)?;
        balance = balance
            .checked_sub(i128::from(output_amount))
            .ok_or(TransferError::BalanceOverflow)?;
        Ok(balance)
    }
}

impl OpId for TransferOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TransferError {
    #[error("Inputs error: {0}")]
    Inputs(#[from] ledger::InputsError),
    #[error("Outputs error: {0}")]
    Outputs(#[from] ledger::OutputsError),
    #[error("Applying this transaction would cause a balance overflow")]
    BalanceOverflow,
    #[error("Invalid transfer ZkSignature")]
    InvalidProof,
}

pub struct TransferValidationContext<'a> {
    pub service_notes: &'a ServiceNotes,
    pub channels: &'a Channels,
    pub utxos: &'a Utxos,
    pub tx_hash_view: &'a TxHashView,
}

impl ProvableOperation for TransferOp {
    type Proof = ZkSignature;
    const CODE: u8 = 0x00;
}

impl OperationGas<MainnetGasProfile> for TransferOp {
    const GAS_COST: Gas = Gas::new(590);
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<TransferOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = TransferError;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Ensure the inputs is non-empty
        operation.inputs.preverify()?;

        // Validate Outputs
        operation.outputs.validate()?;

        Ok(())
    }
}

impl VerifiableOperation<StandardMode> for SignedOperation<TransferOp, Preverified, StandardMode> {
    type Context<'a> = TransferValidationContext<'a>;
    type Error = TransferError;

    fn verify(
        &self,
        context: &Self::Context<'_>,
    ) -> Result<Option<DeferredZkpVerification>, Self::Error> {
        let operation = self.operation();

        // Validate Inputs
        operation.inputs.validate_not_in_channel(
            context.service_notes,
            context.channels,
            context.utxos,
        )?;

        // Defer the proof verification so that the caller can batch it.
        let pks = operation.inputs.get_pk(context.utxos)?;
        let inputs = public_inputs_from_pks((*context.tx_hash_view.as_fr()).into(), &pks)
            .map_err(|_| TransferError::InvalidProof)?;
        Ok(Some(DeferredZkpVerification::ZkSig(
            *self.proof().as_proof(),
            inputs,
        )))
    }
}

impl<Mode: VerificationMode> ExecutableOperation for SignedOperation<TransferOp, Verified, Mode> {
    type Context<'a> = Utxos;
    type Error = TransferError;

    fn execute<'a>(
        &self,
        mut utxos: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // Remove inputs from the ledger
        utxos = operation.inputs.execute(utxos)?;

        // Add outputs from the ledger
        utxos = operation.outputs.execute(utxos, self.operation());
        Ok((utxos, Vec::new()))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<TransferOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
    }
}

#[cfg(test)]
mod test {
    use lb_groth16::CompressedGroth16Proof;
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use lb_poseidon2::Fr;
    use num_bigint::BigUint;

    use super::*;
    use crate::mantle::{Note, NoteId};

    #[test]
    fn test_preverify_rejects_empty_inputs() {
        let pk = ZkPublicKey::from(Fr::from(BigUint::from(0u8)));
        let transfer = TransferOp {
            inputs: Inputs::empty(),
            outputs: Outputs::new([Note::new(100, pk)]),
        };
        let proof = ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128]));
        let signed_operation = SignedOperation::new(transfer, proof);

        assert_eq!(
            signed_operation.preverify(&()),
            Err(TransferError::Inputs(ledger::InputsError::EmptyInputs))
        );
    }

    #[test]
    fn test_utxos_and_utxo_by_index() {
        let pk0 = ZkPublicKey::from(Fr::from(BigUint::from(0u8)));
        let pk1 = ZkPublicKey::from(Fr::from(BigUint::from(1u8)));
        let pk2 = ZkPublicKey::from(Fr::from(BigUint::from(2u8)));
        let transfer = TransferOp {
            inputs: Inputs::new([NoteId(BigUint::from(0u8).into())]),
            outputs: Outputs::new([
                Note::new(100, pk0),
                Note::new(200, pk1),
                Note::new(300, pk2),
            ]),
        };
        assert_eq!(
            transfer.utxo_by_index(0),
            Some(Utxo {
                op_id: transfer.op_id(),
                output_index: 0,
                note: Note::new(100, pk0),
            })
        );
        assert_eq!(
            transfer.utxo_by_index(1),
            Some(Utxo {
                op_id: transfer.op_id(),
                output_index: 1,
                note: Note::new(200, pk1),
            })
        );
        assert_eq!(
            transfer.utxo_by_index(2),
            Some(Utxo {
                op_id: transfer.op_id(),
                output_index: 2,
                note: Note::new(300, pk2),
            })
        );

        assert!(transfer.utxo_by_index(3).is_none());
    }
}
