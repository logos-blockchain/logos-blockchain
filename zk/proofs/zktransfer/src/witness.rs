use lbc_types::native::Witness;

use crate::ZkTransferWitnessInputs;

pub fn generate_witness(inputs: ZkTransferWitnessInputs) -> Result<Witness, lbp_error::Error> {
    let witness_input: lbc_transfer_sys::TransferWitnessInput = inputs.try_into()?;
    lbc_transfer_sys::generate_witness(&witness_input).map_err(Into::into)
}
