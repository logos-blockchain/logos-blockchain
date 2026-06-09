use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::Serialize;

use crate::private::{ZkSignPrivateKeysData, ZkSignPrivateKeysInputs, ZkSignPrivateKeysInputsJson};

pub struct ZkSignWitnessInputs {
    pub msg: Groth16Input,
    pub private_keys: ZkSignPrivateKeysInputs,
}

impl ZkSignWitnessInputs {
    #[must_use]
    pub fn from_witness_data_and_message_hash(private: ZkSignPrivateKeysData, msg: Fr) -> Self {
        Self {
            msg: msg.into(),
            private_keys: private.into(),
        }
    }
}

impl TryFrom<ZkSignWitnessInputs> for lbc_signature_sys::SignatureWitnessInput<'_> {
    type Error = lbp_error::Error;

    fn try_from(value: ZkSignWitnessInputs) -> Result<Self, Self::Error> {
        let inputs_json: ZkSignWitnessInputsJson = (&value).into();
        let inputs_str: String = serde_json::to_string(&inputs_json)?;
        let witness_input = lbc_signature_sys::SignatureWitnessInput::new(inputs_str)?;
        Ok(witness_input)
    }
}

#[derive(Serialize)]
pub struct ZkSignWitnessInputsJson {
    msg: Groth16InputDeser,
    #[serde(rename = "secret_keys")]
    private_keys: ZkSignPrivateKeysInputsJson,
}

impl From<&ZkSignWitnessInputs> for ZkSignWitnessInputsJson {
    fn from(value: &ZkSignWitnessInputs) -> Self {
        Self {
            msg: (&value.msg).into(),
            private_keys: (&value.private_keys).into(),
        }
    }
}
