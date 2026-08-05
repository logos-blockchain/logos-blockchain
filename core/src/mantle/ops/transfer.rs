use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    events::TxEvent,
    mantle::{
        channel::Channels,
        gas::{Gas, MainnetGasConstants, OperationGas},
        ledger::{
            self, ExecutableOperation, Inputs, Outputs, PreverifiableOperation, ProvableOperation,
            Utxo, Utxos, VerifiableOperation, verification_mode,
        },
        ops::OpId,
        transactions::hash::TxHashView,
    },
    sdp::locked_notes::LockedNotes,
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
    #[error("The Transfer Operation doesn't have any input")]
    NoInputTransfer,
    #[error("Applying this transaction would cause a balance overflow")]
    BalanceOverflow,
    #[error("Invalid transfer ZkSignature")]
    InvalidProof,
}

pub struct TransferValidationContext<'a> {
    pub locked_notes: &'a LockedNotes,
    pub channels: &'a Channels,
    pub utxos: &'a Utxos,
    pub tx_hash_view: &'a TxHashView,
}

impl ProvableOperation for TransferOp {
    type Proof = ZkSignature;
}

impl OperationGas<MainnetGasConstants> for TransferOp {
    const GAS_CONSTANT: Gas = Gas::new(590);
}

impl PreverifiableOperation<verification_mode::StandardMode> for TransferOp {
    type Context<'a> = ();
    type Error = TransferError;

    fn preverify(
        &self,
        _proof: &Self::Proof,
        _context: &Self::Context<'_>,
    ) -> Result<(), Self::Error> {
        // Ensure the inputs is non-empty
        if self.inputs.is_empty() {
            return Err(TransferError::NoInputTransfer);
        }

        // Validate Outputs
        self.outputs.validate()?;

        Ok(())
    }
}

impl VerifiableOperation<verification_mode::StandardMode> for TransferOp {
    type Context<'a> = TransferValidationContext<'a>;
    type Error = TransferError;

    fn verify(&self, proof: &Self::Proof, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Validate Inputs
        self.inputs.validate_not_in_channel(
            context.locked_notes,
            context.channels,
            context.utxos,
        )?;

        // Check the transfer Proof
        let pks = self.inputs.get_pk(context.utxos)?;
        if !ZkPublicKey::verify_multi(&pks, context.tx_hash_view.as_fr(), proof) {
            return Err(TransferError::InvalidProof);
        }

        Ok(())
    }
}

impl ExecutableOperation for TransferOp {
    type Context<'a> = Utxos;
    type Error = TransferError;

    fn execute<'a>(
        &self,
        mut utxos: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        // Remove inputs from the ledger
        utxos = self.inputs.execute(utxos)?;
        // Add outputs from the ledger
        utxos = self.outputs.execute(utxos, self);
        Ok((utxos, Vec::new()))
    }
}

#[cfg(test)]
mod test {
    use lb_poseidon2::Fr;
    use num_bigint::BigUint;

    use super::*;
    use crate::mantle::{Note, NoteId};

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
