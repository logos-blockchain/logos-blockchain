use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

use crate::{
    PoCChainInputsData, PoCWalletInputsData,
    chain_inputs::{PoCChainInputs, PoCChainInputsJson},
    wallet_inputs::{PoCWalletInputs, PoCWalletInputsJson},
};

/// The inputs to the circuit prover.
#[derive(Clone, Serialize)]
#[serde(into = "PoCInputsJson", rename_all = "snake_case")]
pub struct PoCWitnessInputs {
    pub wallet: PoCWalletInputs,
    pub chain: PoCChainInputs,
}

impl PoCWitnessInputs {
    #[must_use]
    pub fn from_chain_and_wallet_data(
        chain: PoCChainInputsData,
        wallet: PoCWalletInputsData,
    ) -> Self {
        Self {
            wallet: wallet.into(),
            chain: chain.into(),
        }
    }
}

#[derive(Serialize)]
pub struct PoCInputsJson {
    #[serde(flatten)]
    pub wallet: PoCWalletInputsJson,
    #[serde(flatten)]
    pub chain: PoCChainInputsJson,
}

impl From<&PoCWitnessInputs> for PoCInputsJson {
    fn from(inputs: &PoCWitnessInputs) -> Self {
        Self {
            wallet: (&inputs.wallet).into(),
            chain: (&inputs.chain).into(),
        }
    }
}

impl From<PoCWitnessInputs> for PoCInputsJson {
    fn from(inputs: PoCWitnessInputs) -> Self {
        Self {
            wallet: (&inputs.wallet).into(),
            chain: (&inputs.chain).into(),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct PoCVerifierInputJson([Groth16InputDeser; 3]);

/// Public inputs of the POC verifier circuit as returned by the prover.
/// This inputs are the ones that need to be fed into the verifier.
pub struct PoCVerifierInput {
    voucher_nullifier: Groth16Input,
    voucher_root: Groth16Input,
    mantle_tx_hash: Groth16Input,
}

impl TryFrom<PoCVerifierInputJson> for PoCVerifierInput {
    type Error = <Groth16Input as TryFrom<Groth16InputDeser>>::Error;

    fn try_from(value: PoCVerifierInputJson) -> Result<Self, Self::Error> {
        let [voucher_nullifier, voucher_root, mantle_tx_hash] = value.0;
        Ok(Self {
            voucher_nullifier: voucher_nullifier.try_into()?,
            voucher_root: voucher_root.try_into()?,
            mantle_tx_hash: mantle_tx_hash.try_into()?,
        })
    }
}

impl PoCVerifierInput {
    #[must_use]
    pub const fn to_inputs(&self) -> [Fr; 3] {
        [
            self.voucher_nullifier.into_inner(),
            self.voucher_root.into_inner(),
            self.mantle_tx_hash.into_inner(),
        ]
    }
}
