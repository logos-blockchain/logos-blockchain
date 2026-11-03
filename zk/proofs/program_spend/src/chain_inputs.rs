use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::Serialize;

use crate::{MAX_INPUTS, MAX_OUTPUTS};

#[derive(Copy, Clone)]
pub struct ZkProgramSpendChainInputs {
    inputs_active: [Groth16Input; MAX_INPUTS],
    outputs_active: [Groth16Input; MAX_OUTPUTS],
    outputs_of_program: [Groth16Input; MAX_OUTPUTS],
    tx_commitment_root: Groth16Input,
    program_id: Groth16Input,
    paid_value: Groth16Input,
    msg: Groth16Input,
}

#[derive(Clone, Debug)]
pub struct ZkProgramSpendChainInputsData {
    pub inputs_active: [bool; MAX_INPUTS],
    pub outputs_active: [bool; MAX_OUTPUTS],
    pub outputs_of_program: [bool; MAX_OUTPUTS],
    pub tx_commitment_root: Fr,
    pub program_id: Fr,
    pub paid_value: u64,
    pub msg: Fr,
}

#[derive(Serialize)]
pub struct ZkProgramSpendChainInputsJson {
    inputs_active: [Groth16InputDeser; MAX_INPUTS],
    outputs_active: [Groth16InputDeser; MAX_OUTPUTS],
    outputs_of_program: [Groth16InputDeser; MAX_OUTPUTS],
    #[serde(rename = "tx_cm_root")]
    tx_commitment_root: Groth16InputDeser,
    program_id: Groth16InputDeser,
    paid_value: Groth16InputDeser,
    msg: Groth16InputDeser,
}

impl From<&ZkProgramSpendChainInputs> for ZkProgramSpendChainInputsJson {
    fn from(
        ZkProgramSpendChainInputs {
            inputs_active,
            outputs_active,
            outputs_of_program,
            tx_commitment_root,
            program_id,
            paid_value,
            msg,
        }: &ZkProgramSpendChainInputs,
    ) -> Self {
        Self {
            inputs_active: inputs_active.map(|active| (&active).into()),
            outputs_active: outputs_active.map(|active| (&active).into()),
            outputs_of_program: outputs_of_program.map(|of_program| (&of_program).into()),
            tx_commitment_root: tx_commitment_root.into(),
            program_id: program_id.into(),
            paid_value: paid_value.into(),
            msg: msg.into(),
        }
    }
}

impl From<ZkProgramSpendChainInputsData> for ZkProgramSpendChainInputs {
    fn from(
        ZkProgramSpendChainInputsData {
            inputs_active,
            outputs_active,
            outputs_of_program,
            tx_commitment_root,
            program_id,
            paid_value,
            msg,
        }: ZkProgramSpendChainInputsData,
    ) -> Self {
        Self {
            inputs_active: inputs_active.map(|active| Fr::from(active).into()),
            outputs_active: outputs_active.map(|active| Fr::from(active).into()),
            outputs_of_program: outputs_of_program.map(|of_program| Fr::from(of_program).into()),
            tx_commitment_root: tx_commitment_root.into(),
            program_id: program_id.into(),
            paid_value: Fr::from(paid_value).into(),
            msg: msg.into(),
        }
    }
}
