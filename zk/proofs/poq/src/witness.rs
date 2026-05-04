use lbc_types::native::Bytes;

use crate::PoQWitnessInputs;

pub fn generate_witness(inputs: PoQWitnessInputs) -> Result<Bytes, std::io::Error> {
    let witness_input = inputs.try_into()?;
    lbc_poq_sys::generate_witness(witness_input).map_err(std::io::Error::other)
}
