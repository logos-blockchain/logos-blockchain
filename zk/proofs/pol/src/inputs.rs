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

impl PolWitnessInputs {
    #[must_use]
    pub const fn from_chain_and_wallet_inputs(
        chain: PolChainInputs,
        wallet: PolWalletInputs,
    ) -> Self {
        Self { wallet, chain }
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
pub struct PolVerifierInputJson([Groth16InputDeser; 9]);

/// Public inputs of the POL verifier circuit as returned by the prover.
/// These inputs are the ones that need to be fed into the verifier.
pub struct PolVerifierInput {
    pub entropy_contribution: Groth16Input,
    pub slot_number: Groth16Input,
    pub epoch_nonce: Groth16Input,
    pub lottery_0: Groth16Input,
    pub lottery_1: Groth16Input,
    pub aged_root: Groth16Input,
    pub latest_root: Groth16Input,
    pub leader_pk1: Groth16Input,
    pub leader_pk2: Groth16Input,
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
            aged_root,
            latest_root,
            leader_pk1,
            leader_pk2,
        ] = value.0;
        Ok(Self {
            entropy_contribution: entropy_contribution.try_into()?,
            slot_number: slot_number.try_into()?,
            epoch_nonce: epoch_nonce.try_into()?,
            lottery_0: lottery_0.try_into()?,
            lottery_1: lottery_1.try_into()?,
            aged_root: aged_root.try_into()?,
            latest_root: latest_root.try_into()?,
            leader_pk1: leader_pk1.try_into()?,
            leader_pk2: leader_pk2.try_into()?,
        })
    }
}

impl PolVerifierInput {
    #[must_use]
    pub const fn to_inputs(&self) -> [Fr; 9] {
        [
            self.entropy_contribution.into_inner(),
            self.slot_number.into_inner(),
            self.epoch_nonce.into_inner(),
            self.lottery_0.into_inner(),
            self.lottery_1.into_inner(),
            self.aged_root.into_inner(),
            self.latest_root.into_inner(),
            self.leader_pk1.into_inner(),
            self.leader_pk2.into_inner(),
        ]
    }

    #[must_use]
    pub fn new(
        entropy_contribution: Fr,
        slot_number: u64,
        epoch_nonce: Fr,
        aged_root: Fr,
        latest_root: Fr,
        total_stake: u64,
        leader_pk: (Fr, Fr),
    ) -> Self {
        let (lottery_0, lottery_1) = crate::lottery::compute_lottery_values(total_stake);
        Self {
            entropy_contribution: entropy_contribution.into(),
            slot_number: Fr::from(slot_number).into(),
            epoch_nonce: epoch_nonce.into(),
            lottery_0: Fr::from(lottery_0).into(),
            lottery_1: Fr::from(lottery_1).into(),
            aged_root: aged_root.into(),
            latest_root: latest_root.into(),
            leader_pk1: leader_pk.0.into(),
            leader_pk2: leader_pk.1.into(),
        }
    }
}
