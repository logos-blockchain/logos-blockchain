use lbc_types::native::Bytes;

use crate::PoCWitnessInputs;

pub fn generate_witness(inputs: PoCWitnessInputs) -> Result<Bytes, lbp_error::Error> {
    let witness_input: lbc_poc_sys::PocWitnessInput = inputs.try_into()?;
    lbc_poc_sys::generate_witness(witness_input).map_err(Into::into)
}
