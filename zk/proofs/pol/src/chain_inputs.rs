use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

/// Public inputs of the POL cirmcom circuit with circom specific types.
#[derive(Copy, Clone, Debug)]
pub struct PolChainInputs {
    slot_number: Groth16Input,
    epoch_nonce: Groth16Input,
    lottery_0: Groth16Input,
    lottery_1: Groth16Input,
    eligible_aged: Groth16Input,
    eligible_latest: Groth16Input,
    msg: Groth16Input,
}

/// Public inputs of the POL cirmcom circuit to be provided by the chain.
#[derive(Clone, Debug)]
pub struct PolChainInputsData {
    pub slot_number: u64,
    pub epoch_nonce: Fr,
    pub lottery_0: Fr,
    pub lottery_1: Fr,
    pub eligible_aged: Fr,
    pub eligible_latest: Fr,
    pub msg: Fr,
}

#[derive(Deserialize, Serialize)]
pub struct PolChainInputsJson {
    #[serde(rename = "sl")]
    slot_number: Groth16InputDeser,
    epoch_nonce: Groth16InputDeser,
    #[serde(rename = "t0")]
    lottery_0: Groth16InputDeser,
    #[serde(rename = "t1")]
    lottery_1: Groth16InputDeser,
    eligible_aged: Groth16InputDeser,
    eligible_latest: Groth16InputDeser,
    msg: Groth16InputDeser,
}

impl TryFrom<PolChainInputsJson> for PolChainInputs {
    type Error = <Groth16Input as TryFrom<Groth16InputDeser>>::Error;

    fn try_from(
        PolChainInputsJson {
            slot_number,
            epoch_nonce,
            lottery_0,
            lottery_1,
            eligible_aged,
            eligible_latest,
            msg,
        }: PolChainInputsJson,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            slot_number: slot_number.try_into()?,
            epoch_nonce: epoch_nonce.try_into()?,
            lottery_0: lottery_0.try_into()?,
            lottery_1: lottery_1.try_into()?,
            eligible_aged: eligible_aged.try_into()?,
            eligible_latest: eligible_latest.try_into()?,
            msg: msg.try_into()?,
        })
    }
}

impl From<&PolChainInputs> for PolChainInputsJson {
    fn from(
        PolChainInputs {
            slot_number,
            epoch_nonce,
            lottery_0,
            lottery_1,
            eligible_aged,
            eligible_latest,
            msg,
        }: &PolChainInputs,
    ) -> Self {
        Self {
            slot_number: slot_number.into(),
            epoch_nonce: epoch_nonce.into(),
            lottery_0: lottery_0.into(),
            lottery_1: lottery_1.into(),
            eligible_aged: eligible_aged.into(),
            eligible_latest: eligible_latest.into(),
            msg: msg.into(),
        }
    }
}

impl From<PolChainInputsData> for PolChainInputs {
    fn from(
        PolChainInputsData {
            slot_number,
            epoch_nonce,
            lottery_0,
            lottery_1,
            eligible_aged,
            eligible_latest,
            msg,
        }: PolChainInputsData,
    ) -> Self {
        Self {
            slot_number: Groth16Input::new(slot_number.into()),
            epoch_nonce: Groth16Input::new(epoch_nonce),
            lottery_0: Groth16Input::new(lottery_0),
            lottery_1: Groth16Input::new(lottery_1),
            eligible_aged: eligible_aged.into(),
            eligible_latest: eligible_latest.into(),
            msg: msg.into(),
        }
    }
}
