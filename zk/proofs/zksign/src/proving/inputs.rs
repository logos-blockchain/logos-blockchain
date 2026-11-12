use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use super::private::{ZkSignPrivateKeysData, ZkSignPrivateKeysInputs, ZkSignPrivateKeysInputsJson};
use crate::ZkSignVerifierInputs;

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

#[derive(Deserialize)]
#[serde(transparent)]
pub struct ZkSignVerifierInputsJson(#[serde(with = "BigArray")] [Groth16InputDeser; 33]);

#[derive(Debug, thiserror::Error)]
pub enum ZkSignVerifierInputsJsonTryFromError {
    #[error("Error during deserialization: {0:?}")]
    Groth16DeserError(<Groth16Input as TryFrom<Groth16InputDeser>>::Error),
}

impl TryFrom<ZkSignVerifierInputsJson> for ZkSignVerifierInputs {
    type Error = ZkSignVerifierInputsJsonTryFromError;

    fn try_from(value: ZkSignVerifierInputsJson) -> Result<Self, Self::Error> {
        let [public_key_inputs @ .., msg_input] = value.0;

        let public_keys: [Groth16Input; 32] = public_key_inputs
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()
            .map_err(Self::Error::Groth16DeserError)?
            .try_into()
            .expect("public_key_inputs always contains exactly 32 inputs");

        let msg = msg_input
            .try_into()
            .map_err(Self::Error::Groth16DeserError)?;

        Ok(Self { public_keys, msg })
    }
}
