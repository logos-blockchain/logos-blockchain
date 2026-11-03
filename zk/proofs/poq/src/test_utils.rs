use std::{array, str::FromStr as _};

use lb_groth16::Fr;
use serde_json::Value;

use crate::{
    KeyIndex, NOTE_SET_TREE_DEPTH, PoQChainInputsData, PoQCommonInputsData, PoQSelector,
    PoQWalletInputsData, Quota,
};

pub const LEADER_INPUT_JSON: &str = include_str!("../test_vectors/leader.input.json");

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

fn quota(value: &Value) -> Quota {
    Quota::try_new(u64_value(value)).expect("sample quotas fit in the quota width")
}

fn path_and_selectors(path: &Value, selectors: &Value) -> [(Fr, bool); NOTE_SET_TREE_DEPTH] {
    array::from_fn(|level| (fr(&path[level]), u64_value(&selectors[level]) == 1))
}

#[must_use]
pub fn leader_witness_data() -> (PoQChainInputsData, PoQCommonInputsData, PoQWalletInputsData) {
    let json: Value = serde_json::from_str(LEADER_INPUT_JSON).expect("sample is valid JSON");
    let chain = PoQChainInputsData {
        core_root: fr(&json["core_root"]),
        pol_eligible_aged: fr(&json["pol_eligible_aged"]),
        pol_epoch_nonce: fr(&json["pol_epoch_nonce"]),
        lottery_0: fr(&json["pol_t0"]),
        lottery_1: fr(&json["pol_t1"]),
    };
    let common = PoQCommonInputsData {
        core_quota: quota(&json["core_quota"]),
        leader_quota: quota(&json["leader_quota"]),
        pow_quota: quota(&json["pow_quota"]),
        message_key: (fr(&json["K_part_one"]), fr(&json["K_part_two"])),
        selector: PoQSelector::Leader,
        index: KeyIndex::try_new(u64_value(&json["index"]))
            .expect("sample index fits in the quota width"),
        pow_difficulty: fr(&json["pow_blend_difficulty"]),
    };
    let wallet = PoQWalletInputsData {
        slot: u64_value(&json["pol_sl"]),
        note_value: u64_value(&json["pol_note_value"]),
        note_nonce: fr(&json["pol_note_nonce"]),
        commitment_aged_path_and_selectors: path_and_selectors(
            &json["pol_cm_aged_path"],
            &json["pol_cm_aged_selectors"],
        ),
        nullifier_root_aged: fr(&json["pol_nf_root_aged"]),
        set_aged_path_and_selectors: path_and_selectors(
            &json["pol_set_aged_path"],
            &json["pol_set_selectors"],
        ),
        pol_secret_key: fr(&json["pol_secret_key"]),
    };
    (chain, common, wallet)
}
