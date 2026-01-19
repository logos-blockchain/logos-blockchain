use std::{path::PathBuf, sync::LazyLock};

use crate::{PoCWitnessInputs, inputs::PoCInputsJson};

const CIRCUIT_NAME: &str = "poc";

static BINARY: LazyLock<PathBuf> =
    LazyLock::new(|| lb_circuits_utils::witness_generator_path(CIRCUIT_NAME));

/// Witness of the circuit.
pub struct Witness(Vec<u8>);

impl Witness {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }
}

impl AsRef<[u8]> for Witness {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

pub fn generate_witness(inputs: &PoCWitnessInputs) -> Result<Witness, std::io::Error> {
    let poc_inputs_json: PoCInputsJson = inputs.into();
    let str_inputs: String =
        serde_json::to_string(&poc_inputs_json).expect("Failed to serialize inputs");
    lb_witness_generator::generate_witness(&str_inputs, BINARY.as_path()).map(Witness)
}
