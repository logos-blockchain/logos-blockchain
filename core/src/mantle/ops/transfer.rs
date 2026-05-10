use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    mantle::{
        TxHash,
        encoding::encode_transfer_op,
        ledger,
        ledger::{Inputs, Operation, Outputs, Utxos},
        ops::OpId,
    },
    sdp::locked_notes::LockedNotes,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransferOp {
    pub inputs: Inputs,
    pub outputs: Outputs,
}

impl TransferOp {
    #[must_use]
    pub const fn new(inputs: Inputs, outputs: Outputs) -> Self {
        Self { inputs, outputs }
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
        encode_transfer_op(self)
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
    pub utxos: &'a Utxos,
    pub tx_hash: &'a TxHash,
    pub transfer_sig: &'a ZkSignature,
}

impl Operation<TransferValidationContext<'_>> for TransferOp {
    type ExecutionContext<'a>
        = Utxos
    where
        Self: 'a;
    type Error = TransferError;

    fn validate(&self, ctx: &TransferValidationContext<'_>) -> Result<(), Self::Error> {
        // Ensure the inputs is non-empty
        if self.inputs.is_empty() {
            return Err(TransferError::NoInputTransfer);
        }
        // Validate Inputs
        self.inputs.validate(ctx.locked_notes, ctx.utxos)?;
        // Validate Outputs
        self.outputs.validate()?;
        // Check the transfer Proof
        let pks = self.inputs.get_pk(ctx.utxos)?;
        if !ZkPublicKey::verify_multi(&pks, &ctx.tx_hash.to_fr(), ctx.transfer_sig) {
            return Err(TransferError::InvalidProof);
        }
        Ok(())
    }

    fn execute(
        &self,
        mut utxos: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error> {
        // Remove inputs from the ledger
        utxos = self.inputs.execute(utxos)?;
        // Add outputs from the ledger
        utxos = self.outputs.execute(utxos, self);
        Ok(utxos)
    }
}

#[cfg(test)]
mod test {
    use lb_poseidon2::Fr;
    use num_bigint::BigUint;

    use super::*;
    use crate::mantle::{Note, NoteId, Utxo};

    #[test]
    fn test_utxo_by_index() {
        let pk0 = ZkPublicKey::from(Fr::from(BigUint::from(0u8)));
        let pk1 = ZkPublicKey::from(Fr::from(BigUint::from(1u8)));
        let pk2 = ZkPublicKey::from(Fr::from(BigUint::from(2u8)));
        let transfer = TransferOp {
            inputs: Inputs::new(vec![NoteId(BigUint::from(0u8).into())]),
            outputs: Outputs::new(vec![
                Note::new(100, pk0),
                Note::new(200, pk1),
                Note::new(300, pk2),
            ]),
        };
        assert_eq!(
            transfer.outputs.utxo_by_index(0, &transfer),
            Some(Utxo {
                op_id: transfer.op_id(),
                output_index: 0,
                note: Note::new(100, pk0),
            })
        );
        assert_eq!(
            transfer.outputs.utxo_by_index(1, &transfer),
            Some(Utxo {
                op_id: transfer.op_id(),
                output_index: 1,
                note: Note::new(200, pk1),
            })
        );
        assert_eq!(
            transfer.outputs.utxo_by_index(2, &transfer),
            Some(Utxo {
                op_id: transfer.op_id(),
                output_index: 2,
                note: Note::new(300, pk2),
            })
        );

        assert!(transfer.outputs.utxo_by_index(3, &transfer).is_none());
    }
}
