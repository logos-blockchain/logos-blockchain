use std::{array, iter};

use lb_groth16::{AdditiveGroup as _, Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::{
    MAX_INPUTS, MAX_OUTPUTS, ZkProgramSpendChainInputsData, ZkProgramSpendWalletInputsData,
    chain_inputs::{ZkProgramSpendChainInputs, ZkProgramSpendChainInputsJson},
    wallet_inputs::{ZkProgramSpendWalletInputs, ZkProgramSpendWalletInputsJson},
};

/// The inputs to the circuit prover.
#[derive(Clone, Serialize)]
#[serde(into = "ZkProgramSpendInputsJson", rename_all = "snake_case")]
pub struct ZkProgramSpendWitnessInputs {
    pub wallet: ZkProgramSpendWalletInputs,
    pub chain: ZkProgramSpendChainInputs,
}

impl ZkProgramSpendWitnessInputs {
    #[must_use]
    pub fn from_chain_and_wallet_data(
        chain: ZkProgramSpendChainInputsData,
        wallet: ZkProgramSpendWalletInputsData,
    ) -> Self {
        Self {
            wallet: wallet.into(),
            chain: chain.into(),
        }
    }
}

impl TryFrom<ZkProgramSpendWitnessInputs> for lbc_program_spend_sys::ProgramSpendWitnessInput<'_> {
    type Error = lbp_error::Error;

    fn try_from(value: ZkProgramSpendWitnessInputs) -> Result<Self, Self::Error> {
        let inputs_json: ZkProgramSpendInputsJson = value.into();
        let inputs_str: String = serde_json::to_string(&inputs_json)?;
        let witness_input = lbc_program_spend_sys::ProgramSpendWitnessInput::new(inputs_str)?;
        Ok(witness_input)
    }
}

#[derive(Clone, Debug)]
pub struct ZkProgramSpendWitnessInputsData {
    pub wallet: ZkProgramSpendWalletInputsData,
    pub chain: ZkProgramSpendChainInputsData,
}

impl ZkProgramSpendWitnessInputsData {
    #[must_use]
    pub const fn from_chain_and_wallet_data(
        chain: ZkProgramSpendChainInputsData,
        wallet: ZkProgramSpendWalletInputsData,
    ) -> Self {
        Self { wallet, chain }
    }
}

impl From<ZkProgramSpendWitnessInputsData> for ZkProgramSpendWitnessInputs {
    fn from(
        ZkProgramSpendWitnessInputsData { chain, wallet }: ZkProgramSpendWitnessInputsData,
    ) -> Self {
        Self {
            chain: chain.into(),
            wallet: wallet.into(),
        }
    }
}

#[derive(Serialize)]
pub struct ZkProgramSpendInputsJson {
    #[serde(flatten)]
    pub wallet: ZkProgramSpendWalletInputsJson,
    #[serde(flatten)]
    pub chain: ZkProgramSpendChainInputsJson,
}

impl From<&ZkProgramSpendWitnessInputs> for ZkProgramSpendInputsJson {
    fn from(inputs: &ZkProgramSpendWitnessInputs) -> Self {
        Self {
            wallet: (&inputs.wallet).into(),
            chain: (&inputs.chain).into(),
        }
    }
}

impl From<ZkProgramSpendWitnessInputs> for ZkProgramSpendInputsJson {
    fn from(inputs: ZkProgramSpendWitnessInputs) -> Self {
        Self {
            wallet: (&inputs.wallet).into(),
            chain: (&inputs.chain).into(),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct ZkProgramSpendVerifierInputJson(#[serde(with = "BigArray")] [Groth16InputDeser; 37]);

/// Public inputs of the `ZkProgramSpend` verifier circuit as returned by the
/// prover. This inputs are the ones that need to be fed into the verifier.
#[derive(Clone, Copy, Debug)]
pub struct ZkProgramSpendVerifierInput {
    pub inputs: [Groth16Input; MAX_INPUTS],
    pub outputs: [Groth16Input; MAX_OUTPUTS],
    pub spend_notes_hash: Groth16Input,
    pub inputs_active: [Groth16Input; MAX_INPUTS],
    pub outputs_active: [Groth16Input; MAX_OUTPUTS],
    pub outputs_of_program: [Groth16Input; MAX_OUTPUTS],
    pub tx_commitment_root: Groth16Input,
    pub program_id: Groth16Input,
    pub paid_value: Groth16Input,
    pub msg: Groth16Input,
}

/// The public values of a `ZkProgramSpend` as lists, the verifier input pads
/// them.
pub struct ZkProgramSpendPublic<'a> {
    pub inputs: &'a [Fr],
    pub outputs: &'a [Fr],
    pub outputs_of_program: &'a [bool],
    pub tx_commitment_root: Fr,
    pub program_id: Fr,
    pub paid_value: u64,
    pub spend_notes_hash: Fr,
    pub msg: Fr,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZkProgramSpendInputsError {
    #[error("a ZkProgramSpend spends at most 4 notes: got {0}")]
    TooManyInputs(usize),
    #[error("a ZkProgramSpend creates at most 8 notes: got {0}")]
    TooManyOutputs(usize),
    #[error("{flags} program output flags for {outputs} outputs")]
    ProgramOutputsLength { flags: usize, outputs: usize },
}

impl TryFrom<ZkProgramSpendVerifierInputJson> for ZkProgramSpendVerifierInput {
    type Error = <Groth16Input as TryFrom<Groth16InputDeser>>::Error;

    fn try_from(value: ZkProgramSpendVerifierInputJson) -> Result<Self, Self::Error> {
        let mut signals = [Groth16Input::new(Fr::ZERO); 37];
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
            spend_notes_hash,
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
            output_of_program_0,
            output_of_program_1,
            output_of_program_2,
            output_of_program_3,
            output_of_program_4,
            output_of_program_5,
            output_of_program_6,
            output_of_program_7,
            tx_commitment_root,
            program_id,
            paid_value,
            msg,
        ] = signals;
        Ok(Self {
            inputs: [input_0, input_1, input_2, input_3],
            outputs: [
                output_0, output_1, output_2, output_3, output_4, output_5, output_6, output_7,
            ],
            spend_notes_hash,
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
            outputs_of_program: [
                output_of_program_0,
                output_of_program_1,
                output_of_program_2,
                output_of_program_3,
                output_of_program_4,
                output_of_program_5,
                output_of_program_6,
                output_of_program_7,
            ],
            tx_commitment_root,
            program_id,
            paid_value,
            msg,
        })
    }
}

impl ZkProgramSpendVerifierInput {
    #[must_use]
    pub fn to_inputs(&self) -> [Fr; 37] {
        let signals = self
            .inputs
            .iter()
            .chain(&self.outputs)
            .chain(iter::once(&self.spend_notes_hash))
            .chain(&self.inputs_active)
            .chain(&self.outputs_active)
            .chain(&self.outputs_of_program)
            .chain([
                &self.tx_commitment_root,
                &self.program_id,
                &self.paid_value,
                &self.msg,
            ]);
        let mut inputs = [Fr::ZERO; 37];
        for (input, signal) in inputs.iter_mut().zip(signals) {
            *input = signal.into_inner();
        }
        inputs
    }

    pub fn new(public: &ZkProgramSpendPublic<'_>) -> Result<Self, ZkProgramSpendInputsError> {
        if public.inputs.len() > MAX_INPUTS {
            return Err(ZkProgramSpendInputsError::TooManyInputs(
                public.inputs.len(),
            ));
        }
        if public.outputs.len() > MAX_OUTPUTS {
            return Err(ZkProgramSpendInputsError::TooManyOutputs(
                public.outputs.len(),
            ));
        }
        if public.outputs_of_program.len() != public.outputs.len() {
            return Err(ZkProgramSpendInputsError::ProgramOutputsLength {
                flags: public.outputs_of_program.len(),
                outputs: public.outputs.len(),
            });
        }
        Ok(Self {
            inputs: padded(public.inputs),
            outputs: padded(public.outputs),
            spend_notes_hash: public.spend_notes_hash.into(),
            inputs_active: active_flags(public.inputs.len()),
            outputs_active: active_flags(public.outputs.len()),
            outputs_of_program: padded_flags(public.outputs_of_program),
            tx_commitment_root: public.tx_commitment_root.into(),
            program_id: public.program_id.into(),
            paid_value: Fr::from(public.paid_value).into(),
            msg: public.msg.into(),
        })
    }
}

fn padded<const N: usize>(values: &[Fr]) -> [Groth16Input; N] {
    array::from_fn(|index| values.get(index).copied().unwrap_or(Fr::ZERO).into())
}

fn padded_flags<const N: usize>(flags: &[bool]) -> [Groth16Input; N] {
    array::from_fn(|index| Fr::from(flags.get(index).copied().unwrap_or(false)).into())
}

fn active_flags<const N: usize>(active: usize) -> [Groth16Input; N] {
    array::from_fn(|index| Fr::from(index < active).into())
}
