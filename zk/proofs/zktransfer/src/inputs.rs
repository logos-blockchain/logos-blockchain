use std::{array, iter};

use lb_groth16::{AdditiveGroup as _, Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

use crate::{
    MAX_INPUTS, MAX_OUTPUTS, ZkTransferChainInputsData, ZkTransferWalletInputsData,
    chain_inputs::{ZkTransferChainInputs, ZkTransferChainInputsJson},
    wallet_inputs::{ZkTransferWalletInputs, ZkTransferWalletInputsJson},
};

/// The inputs to the circuit prover.
#[derive(Clone, Serialize)]
#[serde(into = "ZkTransferInputsJson", rename_all = "snake_case")]
pub struct ZkTransferWitnessInputs {
    pub wallet: ZkTransferWalletInputs,
    pub chain: ZkTransferChainInputs,
}

impl ZkTransferWitnessInputs {
    #[must_use]
    pub fn from_chain_and_wallet_data(
        chain: ZkTransferChainInputsData,
        wallet: ZkTransferWalletInputsData,
    ) -> Self {
        Self {
            wallet: wallet.into(),
            chain: chain.into(),
        }
    }
}

impl TryFrom<ZkTransferWitnessInputs> for lbc_transfer_sys::TransferWitnessInput<'_> {
    type Error = lbp_error::Error;

    fn try_from(value: ZkTransferWitnessInputs) -> Result<Self, Self::Error> {
        let inputs_json: ZkTransferInputsJson = value.into();
        let inputs_str: String = serde_json::to_string(&inputs_json)?;
        let witness_input = lbc_transfer_sys::TransferWitnessInput::new(inputs_str)?;
        Ok(witness_input)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZkTransferWitnessInputsData {
    pub wallet: ZkTransferWalletInputsData,
    pub chain: ZkTransferChainInputsData,
}

impl ZkTransferWitnessInputsData {
    #[must_use]
    pub const fn from_chain_and_wallet_data(
        chain: ZkTransferChainInputsData,
        wallet: ZkTransferWalletInputsData,
    ) -> Self {
        Self { wallet, chain }
    }
}

impl From<ZkTransferWitnessInputsData> for ZkTransferWitnessInputs {
    fn from(ZkTransferWitnessInputsData { chain, wallet }: ZkTransferWitnessInputsData) -> Self {
        Self {
            chain: chain.into(),
            wallet: wallet.into(),
        }
    }
}

#[derive(Serialize)]
pub struct ZkTransferInputsJson {
    #[serde(flatten)]
    pub wallet: ZkTransferWalletInputsJson,
    #[serde(flatten)]
    pub chain: ZkTransferChainInputsJson,
}

impl From<&ZkTransferWitnessInputs> for ZkTransferInputsJson {
    fn from(inputs: &ZkTransferWitnessInputs) -> Self {
        Self {
            wallet: (&inputs.wallet).into(),
            chain: (&inputs.chain).into(),
        }
    }
}

impl From<ZkTransferWitnessInputs> for ZkTransferInputsJson {
    fn from(inputs: ZkTransferWitnessInputs) -> Self {
        Self {
            wallet: (&inputs.wallet).into(),
            chain: (&inputs.chain).into(),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct ZkTransferVerifierInputJson([Groth16InputDeser; 27]);

/// Public inputs of the `ZkTransfer` verifier circuit as returned by the
/// prover. This inputs are the ones that need to be fed into the verifier.
#[derive(Clone, Copy, Debug)]
pub struct ZkTransferVerifierInput {
    pub inputs: [Groth16Input; MAX_INPUTS],
    pub outputs: [Groth16Input; MAX_OUTPUTS],
    pub excess_value: Groth16Input,
    pub inputs_active: [Groth16Input; MAX_INPUTS],
    pub outputs_active: [Groth16Input; MAX_OUTPUTS],
    pub tx_commitment_root: Groth16Input,
    pub msg: Groth16Input,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZkTransferInputsError {
    #[error("a ZkTransfer spends at most 4 notes: got {0}")]
    TooManyInputs(usize),
    #[error("a ZkTransfer creates at most 8 notes: got {0}")]
    TooManyOutputs(usize),
}

impl TryFrom<ZkTransferVerifierInputJson> for ZkTransferVerifierInput {
    type Error = <Groth16Input as TryFrom<Groth16InputDeser>>::Error;

    fn try_from(value: ZkTransferVerifierInputJson) -> Result<Self, Self::Error> {
        let mut signals = [Groth16Input::new(Fr::ZERO); 27];
        for (signal, value) in signals.iter_mut().zip(value.0) {
            *signal = value.try_into()?;
        }
        let [
            input_0,
            input_1,
            input_2,
            input_3,
            output_0,
            output_1,
            output_2,
            output_3,
            output_4,
            output_5,
            output_6,
            output_7,
            excess_value,
            input_active_0,
            input_active_1,
            input_active_2,
            input_active_3,
            output_active_0,
            output_active_1,
            output_active_2,
            output_active_3,
            output_active_4,
            output_active_5,
            output_active_6,
            output_active_7,
            tx_commitment_root,
            msg,
        ] = signals;
        Ok(Self {
            inputs: [input_0, input_1, input_2, input_3],
            outputs: [
                output_0, output_1, output_2, output_3, output_4, output_5, output_6, output_7,
            ],
            excess_value,
            inputs_active: [
                input_active_0,
                input_active_1,
                input_active_2,
                input_active_3,
            ],
            outputs_active: [
                output_active_0,
                output_active_1,
                output_active_2,
                output_active_3,
                output_active_4,
                output_active_5,
                output_active_6,
                output_active_7,
            ],
            tx_commitment_root,
            msg,
        })
    }
}

impl ZkTransferVerifierInput {
    #[must_use]
    pub fn to_inputs(&self) -> [Fr; 27] {
        let signals = self
            .inputs
            .iter()
            .chain(&self.outputs)
            .chain(iter::once(&self.excess_value))
            .chain(&self.inputs_active)
            .chain(&self.outputs_active)
            .chain([&self.tx_commitment_root, &self.msg]);
        let mut inputs = [Fr::ZERO; 27];
        for (input, signal) in inputs.iter_mut().zip(signals) {
            *input = signal.into_inner();
        }
        inputs
    }

    pub fn new(
        inputs: &[Fr],
        outputs: &[Fr],
        excess_value: u64,
        tx_commitment_root: Fr,
        msg: Fr,
    ) -> Result<Self, ZkTransferInputsError> {
        if inputs.len() > MAX_INPUTS {
            return Err(ZkTransferInputsError::TooManyInputs(inputs.len()));
        }
        if outputs.len() > MAX_OUTPUTS {
            return Err(ZkTransferInputsError::TooManyOutputs(outputs.len()));
        }
        Ok(Self {
            inputs: padded(inputs),
            outputs: padded(outputs),
            excess_value: Fr::from(excess_value).into(),
            inputs_active: active_flags(inputs.len()),
            outputs_active: active_flags(outputs.len()),
            tx_commitment_root: tx_commitment_root.into(),
            msg: msg.into(),
        })
    }
}

fn padded<const N: usize>(values: &[Fr]) -> [Groth16Input; N] {
    array::from_fn(|index| values.get(index).copied().unwrap_or(Fr::ZERO).into())
}

fn active_flags<const N: usize>(active: usize) -> [Groth16Input; N] {
    array::from_fn(|index| Fr::from(index < active).into())
}
