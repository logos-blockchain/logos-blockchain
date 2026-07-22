use lbc_types::native::Witness;

use crate::PolWitnessInputs;

pub fn generate_witness(inputs: PolWitnessInputs) -> Result<Witness, lbp_error::Error> {
    let witness_input: lbc_pol_sys::PolWitnessInput = inputs.try_into()?;
    lbc_pol_sys::generate_witness(&witness_input).map_err(Into::into)
}
