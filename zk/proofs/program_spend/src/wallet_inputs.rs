use lb_groth16::{AdditiveGroup as _, Fr, Groth16Input, Groth16InputDeser};
use lb_zktransfer::{InputPathAndSelectors, TX_COMMITMENT_TREE_DEPTH};
use serde::Serialize;

use crate::{MAX_INPUTS, MAX_OUTPUTS};

#[derive(Clone)]
pub struct ZkProgramSpendWalletInputs {
    inputs_sk: [Groth16Input; MAX_INPUTS],
    inputs_nonce: [Groth16Input; MAX_INPUTS],
    inputs_value: [Groth16Input; MAX_INPUTS],
    inputs_data: [Groth16Input; MAX_INPUTS],
    inputs_path_and_selectors:
        [[(Groth16Input, Groth16Input); TX_COMMITMENT_TREE_DEPTH]; MAX_INPUTS],
    outputs_pk: [Groth16Input; MAX_OUTPUTS],
    outputs_nonce: [Groth16Input; MAX_OUTPUTS],
    outputs_value: [Groth16Input; MAX_OUTPUTS],
    outputs_program: [Groth16Input; MAX_OUTPUTS],
    outputs_data: [Groth16Input; MAX_OUTPUTS],
}

#[derive(Clone, Debug)]
pub struct ZkProgramSpendInputNoteData {
    pub secret_key: Fr,
    pub nonce: Fr,
    pub value: u64,
    pub data: Fr,
    pub path_and_selectors: InputPathAndSelectors,
}

impl ZkProgramSpendInputNoteData {
    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            secret_key: Fr::ZERO,
            nonce: Fr::ZERO,
            value: 0,
            data: Fr::ZERO,
            path_and_selectors: [(Fr::ZERO, false); TX_COMMITMENT_TREE_DEPTH],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ZkProgramSpendOutputNoteData {
    pub public_key: Fr,
    pub nonce: Fr,
    pub value: u64,
    pub program: Fr,
    pub data: Fr,
}

impl ZkProgramSpendOutputNoteData {
    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            public_key: Fr::ZERO,
            nonce: Fr::ZERO,
            value: 0,
            program: Fr::ZERO,
            data: Fr::ZERO,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ZkProgramSpendWalletInputsData {
    pub inputs: [ZkProgramSpendInputNoteData; MAX_INPUTS],
    pub outputs: [ZkProgramSpendOutputNoteData; MAX_OUTPUTS],
}

#[derive(Serialize)]
pub struct ZkProgramSpendWalletInputsJson {
    inputs_sk: [Groth16InputDeser; MAX_INPUTS],
    inputs_nonce: [Groth16InputDeser; MAX_INPUTS],
    inputs_value: [Groth16InputDeser; MAX_INPUTS],
    inputs_data: [Groth16InputDeser; MAX_INPUTS],
    inputs_selectors: Vec<Vec<Groth16InputDeser>>,
    inputs_path: Vec<Vec<Groth16InputDeser>>,
    outputs_pk: [Groth16InputDeser; MAX_OUTPUTS],
    outputs_nonce: [Groth16InputDeser; MAX_OUTPUTS],
    outputs_value: [Groth16InputDeser; MAX_OUTPUTS],
    outputs_program: [Groth16InputDeser; MAX_OUTPUTS],
    outputs_data: [Groth16InputDeser; MAX_OUTPUTS],
}

impl From<&ZkProgramSpendWalletInputs> for ZkProgramSpendWalletInputsJson {
    fn from(
        ZkProgramSpendWalletInputs {
            inputs_sk,
            inputs_nonce,
            inputs_value,
            inputs_data,
            inputs_path_and_selectors,
            outputs_pk,
            outputs_nonce,
            outputs_value,
            outputs_program,
            outputs_data,
        }: &ZkProgramSpendWalletInputs,
    ) -> Self {
        Self {
            inputs_sk: inputs_sk.map(|sk| (&sk).into()),
            inputs_nonce: inputs_nonce.map(|nonce| (&nonce).into()),
            inputs_value: inputs_value.map(|value| (&value).into()),
            inputs_data: inputs_data.map(|data| (&data).into()),
            inputs_selectors: inputs_path_and_selectors
                .iter()
                .map(|path| path.iter().map(|(_, selector)| selector.into()).collect())
                .collect(),
            inputs_path: inputs_path_and_selectors
                .iter()
                .map(|path| path.iter().map(|(node, _)| node.into()).collect())
                .collect(),
            outputs_pk: outputs_pk.map(|pk| (&pk).into()),
            outputs_nonce: outputs_nonce.map(|nonce| (&nonce).into()),
            outputs_value: outputs_value.map(|value| (&value).into()),
            outputs_program: outputs_program.map(|program| (&program).into()),
            outputs_data: outputs_data.map(|data| (&data).into()),
        }
    }
}

impl From<ZkProgramSpendWalletInputsData> for ZkProgramSpendWalletInputs {
    fn from(
        ZkProgramSpendWalletInputsData { inputs, outputs }: ZkProgramSpendWalletInputsData,
    ) -> Self {
        Self {
            inputs_sk: inputs.each_ref().map(|input| input.secret_key.into()),
            inputs_nonce: inputs.each_ref().map(|input| input.nonce.into()),
            inputs_value: inputs.each_ref().map(|input| Fr::from(input.value).into()),
            inputs_data: inputs.each_ref().map(|input| input.data.into()),
            inputs_path_and_selectors: inputs.each_ref().map(|input| {
                input
                    .path_and_selectors
                    .map(|(node, selector)| (node.into(), Fr::from(selector).into()))
            }),
            outputs_pk: outputs.map(|output| output.public_key.into()),
            outputs_nonce: outputs.map(|output| output.nonce.into()),
            outputs_value: outputs.map(|output| Fr::from(output.value).into()),
            outputs_program: outputs.map(|output| output.program.into()),
            outputs_data: outputs.map(|output| output.data.into()),
        }
    }
}
