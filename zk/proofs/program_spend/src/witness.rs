use lbc_types::native::Witness;

use crate::ZkProgramSpendWitnessInputs;

pub fn generate_witness(inputs: ZkProgramSpendWitnessInputs) -> Result<Witness, lbp_error::Error> {
    let witness_input: lbc_program_spend_sys::ProgramSpendWitnessInput = inputs.try_into()?;
    lbc_program_spend_sys::generate_witness(&witness_input).map_err(Into::into)
}
