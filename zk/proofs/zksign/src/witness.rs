use lbc_types::native::Witness;

use crate::ZkSignWitnessInputs;

pub fn generate_witness(inputs: ZkSignWitnessInputs) -> Result<Witness, lbp_error::Error> {
    let witness_input: lbc_signature_sys::SignatureWitnessInput = inputs.try_into()?;
    lbc_signature_sys::generate_witness(&witness_input).map_err(Into::into)
}
