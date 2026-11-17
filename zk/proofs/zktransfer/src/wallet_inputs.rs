use lb_groth16::{AdditiveGroup as _, Fr, Groth16Input, Groth16InputDeser};
use serde::Serialize;

use crate::{MAX_INPUTS, MAX_OUTPUTS};

pub const TX_COMMITMENT_TREE_DEPTH: usize = 40;
pub type InputPathAndSelectors = [(Fr, bool); TX_COMMITMENT_TREE_DEPTH];

#[derive(Clone)]
pub struct ZkTransferWalletInputs {
    inputs_spending_sk: [Groth16Input; MAX_INPUTS],
    inputs_stake_pk: [Groth16Input; MAX_INPUTS],
    inputs_nonce: [Groth16Input; MAX_INPUTS],
    inputs_value: [Groth16Input; MAX_INPUTS],
    inputs_path_and_selectors:
        [[(Groth16Input, Groth16Input); TX_COMMITMENT_TREE_DEPTH]; MAX_INPUTS],
    outputs_pk: [Groth16Input; MAX_OUTPUTS],
    outputs_stake_pk: [Groth16Input; MAX_OUTPUTS],
    outputs_nonce: [Groth16Input; MAX_OUTPUTS],
    outputs_value: [Groth16Input; MAX_OUTPUTS],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZkTransferInputNoteData {
    // The spending secret key, from which the circuit derives the nullifier key
    // and the public key.
    pub secret_key: Fr,
    // The staking key of the note, which the circuit does not prove.
    pub stake_public_key: Fr,
    pub nonce: Fr,
    pub value: u64,
    pub path_and_selectors: InputPathAndSelectors,
}

impl ZkTransferInputNoteData {
    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            secret_key: Fr::ZERO,
            stake_public_key: Fr::ZERO,
            nonce: Fr::ZERO,
            value: 0,
            path_and_selectors: [(Fr::ZERO, false); TX_COMMITMENT_TREE_DEPTH],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZkTransferOutputNoteData {
    pub public_key: Fr,
    pub stake_public_key: Fr,
    pub nonce: Fr,
    pub value: u64,
}

impl ZkTransferOutputNoteData {
    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            public_key: Fr::ZERO,
            stake_public_key: Fr::ZERO,
            nonce: Fr::ZERO,
            value: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZkTransferWalletInputsData {
    pub inputs: [ZkTransferInputNoteData; MAX_INPUTS],
    pub outputs: [ZkTransferOutputNoteData; MAX_OUTPUTS],
}

#[derive(Serialize)]
pub struct ZkTransferWalletInputsJson {
    inputs_spending_sk: [Groth16InputDeser; MAX_INPUTS],
    inputs_stake_pk: [Groth16InputDeser; MAX_INPUTS],
    inputs_nonce: [Groth16InputDeser; MAX_INPUTS],
    inputs_value: [Groth16InputDeser; MAX_INPUTS],
    inputs_selectors: Vec<Vec<Groth16InputDeser>>,
    inputs_path: Vec<Vec<Groth16InputDeser>>,
    outputs_pk: [Groth16InputDeser; MAX_OUTPUTS],
    outputs_stake_pk: [Groth16InputDeser; MAX_OUTPUTS],
    outputs_nonce: [Groth16InputDeser; MAX_OUTPUTS],
    outputs_value: [Groth16InputDeser; MAX_OUTPUTS],
}

impl From<&ZkTransferWalletInputs> for ZkTransferWalletInputsJson {
    fn from(
        ZkTransferWalletInputs {
            inputs_spending_sk,
            inputs_stake_pk,
            inputs_nonce,
            inputs_value,
            inputs_path_and_selectors,
            outputs_pk,
            outputs_stake_pk,
            outputs_nonce,
            outputs_value,
        }: &ZkTransferWalletInputs,
    ) -> Self {
        Self {
            inputs_spending_sk: inputs_spending_sk.map(|sk| (&sk).into()),
            inputs_stake_pk: inputs_stake_pk.map(|stake_pk| (&stake_pk).into()),
            inputs_nonce: inputs_nonce.map(|nonce| (&nonce).into()),
            inputs_value: inputs_value.map(|value| (&value).into()),
            inputs_selectors: inputs_path_and_selectors
                .iter()
                .map(|path| path.iter().map(|(_, selector)| selector.into()).collect())
                .collect(),
            inputs_path: inputs_path_and_selectors
                .iter()
                .map(|path| path.iter().map(|(node, _)| node.into()).collect())
                .collect(),
            outputs_pk: outputs_pk.map(|pk| (&pk).into()),
            outputs_stake_pk: outputs_stake_pk.map(|stake_pk| (&stake_pk).into()),
            outputs_nonce: outputs_nonce.map(|nonce| (&nonce).into()),
            outputs_value: outputs_value.map(|value| (&value).into()),
        }
    }
}

impl From<ZkTransferWalletInputsData> for ZkTransferWalletInputs {
    fn from(ZkTransferWalletInputsData { inputs, outputs }: ZkTransferWalletInputsData) -> Self {
        Self {
            inputs_spending_sk: inputs.each_ref().map(|input| input.secret_key.into()),
            inputs_stake_pk: inputs.each_ref().map(|input| input.stake_public_key.into()),
            inputs_nonce: inputs.each_ref().map(|input| input.nonce.into()),
            inputs_value: inputs.each_ref().map(|input| Fr::from(input.value).into()),
            inputs_path_and_selectors: inputs.each_ref().map(|input| {
                input
                    .path_and_selectors
                    .map(|(node, selector)| (node.into(), Fr::from(selector).into()))
            }),
            outputs_pk: outputs.map(|output| output.public_key.into()),
            outputs_stake_pk: outputs.map(|output| output.stake_public_key.into()),
            outputs_nonce: outputs.map(|output| output.nonce.into()),
            outputs_value: outputs.map(|output| Fr::from(output.value).into()),
        }
    }
}
