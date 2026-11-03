use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use serde::Serialize;

use crate::{MAX_INPUTS, MAX_OUTPUTS};

#[derive(Copy, Clone)]
pub struct ZkTransferChainInputs {
    inputs_active: [Groth16Input; MAX_INPUTS],
    outputs_active: [Groth16Input; MAX_OUTPUTS],
    tx_commitment_root: Groth16Input,
    msg: Groth16Input,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZkTransferChainInputsData {
    pub inputs_active: [bool; MAX_INPUTS],
    pub outputs_active: [bool; MAX_OUTPUTS],
    pub tx_commitment_root: Fr,
    pub msg: Fr,
}

#[derive(Serialize)]
pub struct ZkTransferChainInputsJson {
    inputs_active: [Groth16InputDeser; MAX_INPUTS],
    outputs_active: [Groth16InputDeser; MAX_OUTPUTS],
    #[serde(rename = "tx_cm_root")]
    tx_commitment_root: Groth16InputDeser,
    msg: Groth16InputDeser,
}

impl From<&ZkTransferChainInputs> for ZkTransferChainInputsJson {
    fn from(
        ZkTransferChainInputs {
            inputs_active,
            outputs_active,
            tx_commitment_root,
            msg,
        }: &ZkTransferChainInputs,
    ) -> Self {
        Self {
            inputs_active: inputs_active.map(|active| (&active).into()),
            outputs_active: outputs_active.map(|active| (&active).into()),
            tx_commitment_root: tx_commitment_root.into(),
            msg: msg.into(),
        }
    }
}

impl From<ZkTransferChainInputsData> for ZkTransferChainInputs {
    fn from(
        ZkTransferChainInputsData {
            inputs_active,
            outputs_active,
            tx_commitment_root,
            msg,
        }: ZkTransferChainInputsData,
    ) -> Self {
        Self {
            inputs_active: inputs_active.map(|active| Fr::from(active).into()),
            outputs_active: outputs_active.map(|active| Fr::from(active).into()),
            tx_commitment_root: tx_commitment_root.into(),
            msg: msg.into(),
        }
    }
}
