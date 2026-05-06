use lbc_types::native::Bytes;

use crate::PoQWitnessInputs;

pub fn generate_witness(inputs: PoQWitnessInputs) -> Result<Bytes, lbp_error::Error> {
    let witness_input: lbc_poq_sys::PoqWitnessInput = inputs.try_into()?;
    lbc_poq_sys::generate_witness(witness_input).map_err(Into::into)
}
