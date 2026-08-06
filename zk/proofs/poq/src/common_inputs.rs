use lb_groth16::{AdditiveGroup as _, Field as _, Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

use crate::{KeyIndex, Quota};

#[derive(Copy, Clone)]
pub struct PoQCommonInputs {
    pub core_quota: Groth16Input,
    pub leader_quota: Groth16Input,
    pub key_part_one: Groth16Input,
    pub key_part_two: Groth16Input,
    pub selector: Groth16Input,
    pub index: Groth16Input,
}

#[derive(Clone, Copy)]
pub struct PoQCommonInputsData {
    pub core_quota: Quota,
    pub leader_quota: Quota,
    pub message_key: (Fr, Fr),
    pub selector: bool,
    pub index: KeyIndex,
}

#[derive(Deserialize, Serialize)]
pub struct PoQCommonInputsJson {
    core_quota: Groth16InputDeser,
    leader_quota: Groth16InputDeser,
    #[serde(rename = "K_part_one")]
    key_part_one: Groth16InputDeser,
    #[serde(rename = "K_part_two")]
    key_part_two: Groth16InputDeser,
    selector: Groth16InputDeser,
    index: Groth16InputDeser,
}

impl From<&PoQCommonInputs> for PoQCommonInputsJson {
    fn from(
        PoQCommonInputs {
            core_quota,
            leader_quota,
            key_part_one,
            key_part_two,
            selector,
            index,
        }: &PoQCommonInputs,
    ) -> Self {
        Self {
            core_quota: core_quota.into(),
            leader_quota: leader_quota.into(),
            key_part_one: key_part_one.into(),
            key_part_two: key_part_two.into(),
            selector: selector.into(),
            index: index.into(),
        }
    }
}

impl From<PoQCommonInputsData> for PoQCommonInputs {
    fn from(
        PoQCommonInputsData {
            core_quota,
            leader_quota,
            message_key,
            selector,
            index,
        }: PoQCommonInputsData,
    ) -> Self {
        Self {
            core_quota: core_quota.into(),
            leader_quota: leader_quota.into(),
            key_part_one: message_key.0.into(),
            key_part_two: message_key.1.into(),
            selector: Groth16Input::new(if selector { Fr::ONE } else { Fr::ZERO }),
            index: index.into(),
        }
    }
}
