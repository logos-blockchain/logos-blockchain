use lb_groth16::{AdditiveGroup as _, Fr, Groth16Input, Groth16InputDeser};
use serde::Deserialize;
use serde_big_array::BigArray;

#[derive(Clone, Debug)]
pub struct ZkSignVerifierInputs {
    pub public_keys: [Groth16Input; 32],
    pub msg: Groth16Input,
}

impl ZkSignVerifierInputs {
    pub fn as_inputs(&self) -> [Fr; 33] {
        let mut buff = [Fr::ZERO; 33];
        buff[..32].copy_from_slice(self.public_keys.map(Groth16Input::into_inner).as_ref());
        buff[32] = self.msg.into_inner();
        buff
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
