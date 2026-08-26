use lb_codec::{BinaryCodec, BinaryEncode as _};
#[cfg(any(test, feature = "samples"))]
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(any(test, feature = "samples"))]
use crate::mantle::{Note, NoteId};
use crate::{
    events::TxEvent,
    mantle::{
        Value,
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

    #[cfg(any(test, feature = "samples"))]
    #[must_use]
    pub fn sample() -> Self {
        Self::new(
            Inputs::new([NoteId(Fr::from(1u64)), NoteId(Fr::from(2u64))]),
            Outputs::new([
                Note::new(3, ZkPublicKey::from(Fr::from(4u64))),
                Note::new(5, ZkPublicKey::from(Fr::from(6u64))),
            ]),
        )
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
    pub locked_notes: &'a LockedNotes,
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

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Validate Inputs
        operation.inputs.validate_not_in_channel(
            context.locked_notes,
            context.channels,
            context.utxos,
        )?;

        // Check the transfer Proof
        let pks = operation.inputs.get_pk(context.utxos)?;
        if !ZkPublicKey::verify_multi(&pks, context.tx_hash_view.as_fr(), self.proof()) {
            return Err(TransferError::InvalidProof);
        }

        Ok(())
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
    use lb_key_management_system_keys::keys::ZkKey;
    use num_bigint::BigUint;

    use super::*;
    use crate::mantle::{
        TxHash,
        ops::{channel::ChannelId, op_proof::samples::SampleProof as _},
    };

    #[test]
    fn preverify_rejects_empty_inputs() {
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
    fn preverify_rejects_a_zero_value_output() {
        let pk = ZkPublicKey::from(Fr::from(BigUint::from(0u8)));
        let transfer = TransferOp {
            inputs: Inputs::new([NoteId(Fr::from(BigUint::from(1u8)))]),
            outputs: Outputs::new([Note::new(0, pk)]),
        };
        let proof = ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128]));
        let signed_operation = SignedOperation::new(transfer, proof);

        assert_eq!(
            signed_operation.preverify(&()),
            Err(TransferError::Outputs(ledger::OutputsError::ZeroValueNote))
        );
    }

    #[test]
    fn utxos_and_utxo_by_index() {
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

    #[test]
    fn verify_rejects_a_proof_over_another_transaction() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };
        let (utxos, _) = Utxos::new().insert(input_utxo.id(), input_utxo);

        let operation = TransferOp::new(
            Inputs::new([input_utxo.id()]),
            Outputs::new([Note::new(
                10_000,
                ZkPublicKey::from(Fr::from(BigUint::from(2u8))),
            )]),
        );

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let other_view = TxHashView::from(TxHash::from([10u8; 32]));
        let proof =
            ZkKey::multi_sign(&[input_key], signed_view.as_fr()).expect("signing should succeed");

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify should accept a well-formed transfer");

        let channels = Channels::new();
        let locked_notes = LockedNotes::new();

        assert_eq!(
            signed_operation.verify(&TransferValidationContext {
                locked_notes: &locked_notes,
                channels: &channels,
                utxos: &utxos,
                tx_hash_view: &signed_view,
            }),
            Ok(())
        );
        assert_eq!(
            signed_operation.verify(&TransferValidationContext {
                locked_notes: &locked_notes,
                channels: &channels,
                utxos: &utxos,
                tx_hash_view: &other_view,
            }),
            Err(TransferError::InvalidProof)
        );
    }

    #[test]
    fn verify_rejects_a_proof_from_a_key_that_does_not_own_the_input() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let other_key = ZkKey::from(BigUint::from(7u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };
        let (utxos, _) = Utxos::new().insert(input_utxo.id(), input_utxo);

        let operation = TransferOp::new(
            Inputs::new([input_utxo.id()]),
            Outputs::new([Note::new(
                10_000,
                ZkPublicKey::from(Fr::from(BigUint::from(2u8))),
            )]),
        );

        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            ZkKey::multi_sign(&[other_key], tx_hash_view.as_fr()).expect("signing should succeed");

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify should accept a well-formed transfer");

        assert_eq!(
            signed_operation.verify(&TransferValidationContext {
                locked_notes: &LockedNotes::new(),
                channels: &Channels::new(),
                utxos: &utxos,
                tx_hash_view: &tx_hash_view,
            }),
            Err(TransferError::InvalidProof)
        );
    }

    #[test]
    fn verify_rejects_an_input_missing_from_the_ledger() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };

        let operation = TransferOp::new(
            Inputs::new([input_utxo.id()]),
            Outputs::new([Note::new(
                10_000,
                ZkPublicKey::from(Fr::from(BigUint::from(2u8))),
            )]),
        );

        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            ZkKey::multi_sign(&[input_key], tx_hash_view.as_fr()).expect("signing should succeed");

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify should accept a well-formed transfer");

        assert_eq!(
            signed_operation.verify(&TransferValidationContext {
                locked_notes: &LockedNotes::new(),
                channels: &Channels::new(),
                utxos: &Utxos::new(),
                tx_hash_view: &tx_hash_view,
            }),
            Err(TransferError::Inputs(ledger::InputsError::InexistingNote(
                input_utxo.id()
            )))
        );
    }

    #[test]
    fn verify_rejects_an_input_owned_by_a_channel() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };
        let (utxos, _) = Utxos::new().insert(input_utxo.id(), input_utxo);

        let operation = TransferOp::new(
            Inputs::new([input_utxo.id()]),
            Outputs::new([Note::new(
                10_000,
                ZkPublicKey::from(Fr::from(BigUint::from(2u8))),
            )]),
        );

        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            ZkKey::multi_sign(&[input_key], tx_hash_view.as_fr()).expect("signing should succeed");

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify should accept a well-formed transfer");

        let channels = Channels::new()
            .register_channel_note(&input_utxo.id(), &ChannelId::from([21u8; 32]))
            .expect("the note is not owned by another channel");

        assert_eq!(
            signed_operation.verify(&TransferValidationContext {
                locked_notes: &LockedNotes::new(),
                channels: &channels,
                utxos: &utxos,
                tx_hash_view: &tx_hash_view,
            }),
            Err(TransferError::Inputs(ledger::InputsError::ChannelNote(
                input_utxo.id()
            )))
        );
    }

    #[test]
    fn execute_rejects_an_input_missing_from_the_ledger() {
        let missing_note = NoteId(Fr::from(BigUint::from(3u8)));
        let operation = TransferOp::new(
            Inputs::new([missing_note]),
            Outputs::new([Note::new(
                10_000,
                ZkPublicKey::from(Fr::from(BigUint::from(2u8))),
            )]),
        );

        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            operation,
            <TransferOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted();

        assert_eq!(
            signed_operation
                .execute(Utxos::new())
                .map(|_| ())
                .map_err(|(_, error)| error),
            Err(TransferError::Inputs(ledger::InputsError::InexistingNote(
                missing_note
            )))
        );
    }

    #[test]
    fn execute_removes_the_inputs_and_adds_the_outputs() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };
        let (utxos, _) = Utxos::new().insert(input_utxo.id(), input_utxo);

        let operation = TransferOp::new(
            Inputs::new([input_utxo.id()]),
            Outputs::new([Note::new(
                10_000,
                ZkPublicKey::from(Fr::from(BigUint::from(2u8))),
            )]),
        );
        let output_utxo = operation
            .utxo_by_index(0)
            .expect("the operation declares one output");

        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            operation,
            <TransferOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted();

        let (utxos, events) = signed_operation
            .execute(utxos)
            .expect("the input is in the ledger");

        assert!(!utxos.contains(&input_utxo.id()));
        assert_eq!(utxos.get(&output_utxo.id()), Some(output_utxo));
        assert!(events.is_empty());
    }

    #[test]
    fn transfer_op_execution_gas() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            TransferOp::sample(),
            <TransferOp as ProvableOperation>::Proof::sample(),
        );

        assert_eq!(
            signed_operation.execution_gas::<MainnetGasProfile>(),
            Ok(Gas::new(590))
        );
    }
}
