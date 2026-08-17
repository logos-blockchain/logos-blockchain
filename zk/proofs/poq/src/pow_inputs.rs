use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct PoQPowInputs {
    pub pow_nonce: Groth16Input,
}

pub struct PoQPowInputsData {
    pub pow_nonce: Fr,
}

#[derive(Deserialize, Serialize)]
pub struct PoQPowInputsJson {
    #[serde(rename = "pow_nonce")]
    pow_nonce: Groth16InputDeser,
}

impl From<PoQPowInputs> for PoQPowInputsJson {
    fn from(PoQPowInputs { pow_nonce }: PoQPowInputs) -> Self {
        Self {
            pow_nonce: (&pow_nonce).into(),
        }
    }
}

impl From<PoQPowInputsData> for PoQPowInputs {
    fn from(PoQPowInputsData { pow_nonce }: PoQPowInputsData) -> Self {
        Self {
            pow_nonce: pow_nonce.into(),
        }
    }
}
