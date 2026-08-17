use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

use crate::{KeyIndex, Quota};

#[derive(Copy, Clone)]
pub struct PoQCommonInputs {
    pub core_quota: Groth16Input,
    pub leader_quota: Groth16Input,
    pub pow_quota: Groth16Input,
    pub key_part_one: Groth16Input,
    pub key_part_two: Groth16Input,
    pub selector: Groth16Input,
    pub index: Groth16Input,
    pub pow_difficulty: Groth16Input,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PoQSelector {
    Core = 0,
    Leader = 1,
    Pow = 2,
}

#[derive(Clone, Copy)]
pub struct PoQCommonInputsData {
    pub core_quota: Quota,
    pub leader_quota: Quota,
    pub pow_quota: Quota,
    pub message_key: (Fr, Fr),
    pub selector: PoQSelector,
    pub index: KeyIndex,
    pub pow_difficulty: Fr,
}

#[derive(Deserialize, Serialize)]
pub struct PoQCommonInputsJson {
    core_quota: Groth16InputDeser,
    leader_quota: Groth16InputDeser,
    pow_quota: Groth16InputDeser,
    #[serde(rename = "K_part_one")]
    key_part_one: Groth16InputDeser,
    #[serde(rename = "K_part_two")]
    key_part_two: Groth16InputDeser,
    selector: Groth16InputDeser,
    index: Groth16InputDeser,
    #[serde(rename = "pow_blend_difficulty")]
    pow_difficulty: Groth16InputDeser,
}

impl From<&PoQCommonInputs> for PoQCommonInputsJson {
    fn from(
        PoQCommonInputs {
            core_quota,
            leader_quota,
            pow_quota,
            key_part_one,
            key_part_two,
            selector,
            index,
            pow_difficulty,
        }: &PoQCommonInputs,
    ) -> Self {
        Self {
            core_quota: core_quota.into(),
            leader_quota: leader_quota.into(),
            pow_quota: pow_quota.into(),
            key_part_one: key_part_one.into(),
            key_part_two: key_part_two.into(),
            selector: selector.into(),
            index: index.into(),
            pow_difficulty: pow_difficulty.into(),
        }
    }
}

impl From<PoQCommonInputsData> for PoQCommonInputs {
    fn from(
        PoQCommonInputsData {
            core_quota,
            leader_quota,
            pow_quota,
            message_key,
            selector,
            index,
            pow_difficulty,
        }: PoQCommonInputsData,
    ) -> Self {
        Self {
            core_quota: core_quota.into(),
            leader_quota: leader_quota.into(),
            pow_quota: pow_quota.into(),
            key_part_one: message_key.0.into(),
            key_part_two: message_key.1.into(),
            selector: Groth16Input::new(Fr::from(BigUint::from(selector as u8))),
            index: index.into(),
            pow_difficulty: pow_difficulty.into(),
        }
    }
}
