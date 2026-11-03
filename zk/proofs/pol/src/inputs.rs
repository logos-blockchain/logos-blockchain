use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

use crate::{
    PolChainInputsData, PolWalletInputsData,
    chain_inputs::{PolChainInputs, PolChainInputsJson},
    wallet_inputs::{PolWalletInputs, PolWalletInputsJson},
};

/// The inputs to the circuit prover.
#[derive(Clone, Serialize, Debug)]
#[serde(into = "PolInputsJson", rename_all = "snake_case")]
pub struct PolWitnessInputs {
    pub wallet: PolWalletInputs,
    pub chain: PolChainInputs,
}

impl TryFrom<PolWitnessInputs> for lbc_pol_sys::PolWitnessInput<'_> {
    type Error = lbp_error::Error;

    fn try_from(value: PolWitnessInputs) -> Result<Self, Self::Error> {
        let inputs_json: PolInputsJson = value.into();
        let inputs_str: String = serde_json::to_string(&inputs_json)?;
        let witness_input = lbc_pol_sys::PolWitnessInput::new(inputs_str)?;
        Ok(witness_input)
    }
}

#[derive(Clone, Debug)]
pub struct PolWitnessInputsData {
    pub wallet: PolWalletInputsData,
    pub chain: PolChainInputsData,
}

impl PolWitnessInputsData {
    #[must_use]
    pub const fn from_chain_and_wallet_data(
        chain: PolChainInputsData,
        wallet: PolWalletInputsData,
    ) -> Self {
        Self { wallet, chain }
    }
}

impl From<PolWitnessInputsData> for PolWitnessInputs {
    fn from(PolWitnessInputsData { chain, wallet }: PolWitnessInputsData) -> Self {
        Self {
            chain: chain.into(),
            wallet: wallet.into(),
        }
    }
}

#[derive(Serialize)]
pub struct PolInputsJson {
    #[serde(flatten)]
    pub wallet: PolWalletInputsJson,
    #[serde(flatten)]
    pub chain: PolChainInputsJson,
}

impl From<&PolWitnessInputs> for PolInputsJson {
    fn from(inputs: &PolWitnessInputs) -> Self {
        Self {
            wallet: (&inputs.wallet).into(),
            chain: (&inputs.chain).into(),
        }
    }
}

impl From<PolWitnessInputs> for PolInputsJson {
    fn from(inputs: PolWitnessInputs) -> Self {
        Self {
            wallet: (&inputs.wallet).into(),
            chain: (&inputs.chain).into(),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct PolVerifierInputJson([Groth16InputDeser; 8]);

/// Public inputs of the POL verifier circuit as returned by the prover.
/// These inputs are the ones that need to be fed into the verifier.
pub struct PolVerifierInput {
    pub entropy_contribution: Groth16Input,
    pub slot_number: Groth16Input,
    pub epoch_nonce: Groth16Input,
    pub lottery_0: Groth16Input,
    pub lottery_1: Groth16Input,
    pub eligible_aged: Groth16Input,
    pub eligible_latest: Groth16Input,
    pub msg: Groth16Input,
}

impl TryFrom<PolVerifierInputJson> for PolVerifierInput {
    type Error = <Groth16Input as TryFrom<Groth16InputDeser>>::Error;

    fn try_from(value: PolVerifierInputJson) -> Result<Self, Self::Error> {
        let [
            entropy_contribution,
            slot_number,
            epoch_nonce,
            lottery_0,
            lottery_1,
            eligible_aged,
            eligible_latest,
            msg,
        ] = value.0;
        Ok(Self {
            entropy_contribution: entropy_contribution.try_into()?,
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

impl PolVerifierInput {
    #[must_use]
    pub const fn to_inputs(&self) -> [Fr; 8] {
        [
            self.entropy_contribution.into_inner(),
            self.slot_number.into_inner(),
            self.epoch_nonce.into_inner(),
            self.lottery_0.into_inner(),
            self.lottery_1.into_inner(),
            self.eligible_aged.into_inner(),
            self.eligible_latest.into_inner(),
            self.msg.into_inner(),
        ]
    }

    #[must_use]
    #[expect(clippy::too_many_arguments, reason = "all needed")]
    pub fn new(
        entropy_contribution: Fr,
        slot_number: u64,
        epoch_nonce: Fr,
        eligible_aged: Fr,
        eligible_latest: Fr,
        lottery_0: Fr,
        lottery_1: Fr,
        msg: Fr,
    ) -> Self {
        Self {
            entropy_contribution: entropy_contribution.into(),
            slot_number: Fr::from(slot_number).into(),
            epoch_nonce: epoch_nonce.into(),
            lottery_0: lottery_0.into(),
            lottery_1: lottery_1.into(),
            eligible_aged: eligible_aged.into(),
            eligible_latest: eligible_latest.into(),
            msg: msg.into(),
        }
    }
}
