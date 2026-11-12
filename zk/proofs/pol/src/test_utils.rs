use std::{array, str::FromStr as _};

use lb_groth16::Fr;
use serde_json::Value;

use crate::{NOTE_SET_TREE_DEPTH, PolChainInputsData, PolWalletInputsData, PolWitnessInputsData};

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

fn path(value: &Value) -> [Fr; NOTE_SET_TREE_DEPTH] {
    array::from_fn(|level| fr(&value[level]))
}

fn selectors(value: &Value) -> [bool; NOTE_SET_TREE_DEPTH] {
    array::from_fn(|level| flag(&value[level]))
}

#[must_use]
pub fn sample_witness_data() -> PolWitnessInputsData {
    let json: Value = serde_json::from_str(SAMPLE_INPUT_JSON).expect("sample is valid JSON");
    let chain = PolChainInputsData {
        slot_number: u64_value(&json["sl"]),
        epoch_nonce: fr(&json["epoch_nonce"]),
        lottery_0: fr(&json["t0"]),
        lottery_1: fr(&json["t1"]),
        eligible_aged: fr(&json["eligible_aged"]),
        eligible_latest: fr(&json["eligible_latest"]),
        msg: fr(&json["msg"]),
        reward_value: u64_value(&json["reward_value"]),
    };
    let wallet = PolWalletInputsData {
        note_value: u64_value(&json["value"]),
        note_nonce: fr(&json["nonce"]),
        note_program: fr(&json["program"]),
        note_data: fr(&json["data"]),
        set_selectors: selectors(&json["set_selectors"]),
        commitment_aged_path: path(&json["cm_aged_path"]),
        commitment_aged_selectors: selectors(&json["cm_aged_selectors"]),
        nullifier_root_aged: fr(&json["nf_root_aged"]),
        low_leaf_aged_nullifier: Fr::from(0u64),
        low_leaf_aged_next_nullifier: Fr::from(0u64),
        low_leaf_aged_path: [Fr::from(0u64); NOTE_SET_TREE_DEPTH],
        low_leaf_aged_selectors: [false; NOTE_SET_TREE_DEPTH],
        set_aged_path: path(&json["set_aged_path"]),
        low_leaf_nullifier: fr(&json["low_leaf_nf"]),
        low_leaf_next_nullifier: fr(&json["low_leaf_next_nf"]),
        low_leaf_path: path(&json["low_leaf_path"]),
        low_leaf_selectors: selectors(&json["low_leaf_selectors"]),
        commitment_root_latest: fr(&json["cm_root_latest"]),
        set_latest_path: path(&json["set_latest_path"]),
        nullifier_key: fr(&json["nf_sk"]),
        staking_secret_key: fr(&json["staking_sk"]),
        reward_public_key: fr(&json["reward_public_key"]),
        reward_nonce: fr(&json["reward_nonce"]),
    };
    PolWitnessInputsData::from_chain_and_wallet_data(chain, wallet)
}
