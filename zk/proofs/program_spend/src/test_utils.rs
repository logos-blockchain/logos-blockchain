use std::{array, str::FromStr as _};

use lb_groth16::Fr;
use serde_json::Value;

use crate::{
    ZkProgramSpendChainInputsData, ZkProgramSpendInputNoteData, ZkProgramSpendOutputNoteData,
    ZkProgramSpendWalletInputsData, ZkProgramSpendWitnessInputsData,
};

pub const SAMPLE_INPUT_JSON: &str = include_str!("../test_vectors/sample.input.json");

fn decimal(value: &Value) -> &str {
    value.as_str().expect("sample numbers are decimal strings")
}

fn fr(value: &Value) -> Fr {
    Fr::from_str(decimal(value)).expect("sample field elements are canonical")
}

fn u64_value(value: &Value) -> u64 {
    decimal(value)
        .parse::<u64>()
        .expect("sample values fit in u64")
}

fn flag(value: &Value) -> bool {
    u64_value(value) == 1
}

#[must_use]
pub fn sample_witness_data() -> ZkProgramSpendWitnessInputsData {
    let json: Value = serde_json::from_str(SAMPLE_INPUT_JSON).expect("sample is valid JSON");
    let wallet = ZkProgramSpendWalletInputsData {
        inputs: array::from_fn(|i| ZkProgramSpendInputNoteData {
            secret_key: fr(&json["inputs_sk"][i]),
            nonce: fr(&json["inputs_nonce"][i]),
            value: u64_value(&json["inputs_value"][i]),
            data: fr(&json["inputs_data"][i]),
            path_and_selectors: array::from_fn(|j| {
                (
                    fr(&json["inputs_path"][i][j]),
                    flag(&json["inputs_selectors"][i][j]),
                )
            }),
        }),
        outputs: array::from_fn(|i| ZkProgramSpendOutputNoteData {
            public_key: fr(&json["outputs_pk"][i]),
            nonce: fr(&json["outputs_nonce"][i]),
            value: u64_value(&json["outputs_value"][i]),
            program: fr(&json["outputs_program"][i]),
            data: fr(&json["outputs_data"][i]),
        }),
    };
    let chain = ZkProgramSpendChainInputsData {
        inputs_active: array::from_fn(|i| flag(&json["inputs_active"][i])),
        outputs_active: array::from_fn(|i| flag(&json["outputs_active"][i])),
        outputs_of_program: array::from_fn(|i| flag(&json["outputs_of_program"][i])),
        tx_commitment_root: fr(&json["tx_cm_root"]),
        program_id: fr(&json["program_id"]),
        paid_value: u64_value(&json["paid_value"]),
        msg: fr(&json["msg"]),
    };
    ZkProgramSpendWitnessInputsData::from_chain_and_wallet_data(chain, wallet)
}
