use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct PoQPowInputs {
    pub pow_secret_key: Groth16Input,
    pub block_hash: Groth16Input,
}

pub struct PoQPowInputsData {
    pub pow_secret_key: Fr,
    pub block_hash: Fr,
}

#[derive(Deserialize, Serialize)]
pub struct PoQPowInputsJson {
    #[serde(rename = "pow_sk")]
    pow_secret_key: Groth16InputDeser,
    #[serde(rename = "pow_block_hash")]
    block_hash: Groth16InputDeser,
}

impl From<PoQPowInputs> for PoQPowInputsJson {
    fn from(
        PoQPowInputs {
            pow_secret_key,
            block_hash,
        }: PoQPowInputs,
    ) -> Self {
        Self {
            pow_secret_key: (&pow_secret_key).into(),
            block_hash: (&block_hash).into(),
        }
    }
}

impl From<PoQPowInputsData> for PoQPowInputs {
    fn from(
        PoQPowInputsData {
            pow_secret_key,
            block_hash,
        }: PoQPowInputsData,
    ) -> Self {
        Self {
            pow_secret_key: pow_secret_key.into(),
            block_hash: block_hash.into(),
        }
    }
}
