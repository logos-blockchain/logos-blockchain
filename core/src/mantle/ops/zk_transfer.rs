//! The checks and the deferred proof shared by the Operations proven by a
//! `ZkTransfer`.

use lb_groth16::Fr;
use lb_zktransfer::{MAX_INPUTS, MAX_OUTPUTS, ZkTransferInputsError, ZkTransferVerifierInput};

use crate::mantle::{
    batch::DeferredZkTransfer,
    ledger::Value,
    note_set::NoteSetError,
    notes::{InputNullifiers, NoteCommitment, NoteNullifier},
    ops::ZkTransferProof,
};

/// The checks on the note lists of an Operation that need no state.
pub trait NoteListChecks {
    /// Fails if the Operation spends no note.
    fn check_non_empty(&self) -> Result<(), NoteSetError>;
}

impl NoteListChecks for InputNullifiers {
    fn check_non_empty(&self) -> Result<(), NoteSetError> {
        if self.is_empty() {
            return Err(NoteSetError::EmptyInputs);
        }
        Ok(())
    }
}

/// Fails if the `ZkTransfer` circuit does not fit `inputs` input notes and
/// `outputs` output notes.
pub const fn check_zk_transfer_bounds(
    inputs: usize,
    outputs: usize,
) -> Result<(), ZkTransferInputsError> {
    if inputs > MAX_INPUTS {
        return Err(ZkTransferInputsError::TooManyInputs(inputs));
    }
    if outputs > MAX_OUTPUTS {
        return Err(ZkTransferInputsError::TooManyOutputs(outputs));
    }
    Ok(())
}

/// The preverification of an Operation spending `inputs` into `outputs`
/// through a `ZkTransfer`: it spends at least one note, and the circuit fits
/// its notes.
pub fn preverify_zk_transfer<E>(inputs: &InputNullifiers, outputs: usize) -> Result<(), E>
where
    E: From<NoteSetError> + From<ZkTransferInputsError>,
{
    inputs.check_non_empty()?;
    check_zk_transfer_bounds(inputs.len(), outputs)?;
    Ok(())
}

/// The `ZkTransfer` of an Operation, deferred so that the caller can batch it.
pub fn defer_zk_transfer<'a>(
    proof: &ZkTransferProof,
    inputs: impl IntoIterator<Item = &'a NoteNullifier>,
    outputs: impl IntoIterator<Item = &'a NoteCommitment>,
    excess_value: Value,
    tx_commitment_root: Fr,
    msg: Fr,
) -> Result<DeferredZkTransfer, ZkTransferInputsError> {
    let nullifiers: Vec<Fr> = inputs.into_iter().map(|nullifier| nullifier.0).collect();
    let commitments: Vec<Fr> = outputs.into_iter().map(|commitment| commitment.0).collect();
    let public = ZkTransferVerifierInput::new(
        &nullifiers,
        &commitments,
        excess_value,
        tx_commitment_root,
        msg,
    )?;
    Ok(DeferredZkTransfer::new(*proof.as_proof(), public))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_reject_more_notes_than_the_circuit_fits() {
        assert_eq!(check_zk_transfer_bounds(MAX_INPUTS, MAX_OUTPUTS), Ok(()));
        assert_eq!(
            check_zk_transfer_bounds(MAX_INPUTS + 1, 0),
            Err(ZkTransferInputsError::TooManyInputs(MAX_INPUTS + 1))
        );
        assert_eq!(
            check_zk_transfer_bounds(0, MAX_OUTPUTS + 1),
            Err(ZkTransferInputsError::TooManyOutputs(MAX_OUTPUTS + 1))
        );
    }

    #[test]
    fn no_input_is_rejected() {
        assert_eq!(
            InputNullifiers::default().check_non_empty(),
            Err(NoteSetError::EmptyInputs)
        );
    }
}
