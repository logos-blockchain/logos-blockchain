use lb_groth16::{Fr, Groth16Input, Groth16InputDeser};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

use crate::{
    PoQInputsFromDataError::PowQuotaMoreThan20Bits,
    chain_inputs::{
        PoQInputsFromDataError,
        PoQInputsFromDataError::{CoreQuotaMoreThan20Bits, LeaderQuotaMoreThan20Bits},
    },
};

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
    pub core_quota: u64,
    pub leader_quota: u64,
    pub pow_quota: u64,
    pub message_key: (Fr, Fr),
    pub selector: PoQSelector,
    pub index: u64,
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

impl TryFrom<PoQCommonInputsData> for PoQCommonInputs {
    type Error = PoQInputsFromDataError;
    fn try_from(
        PoQCommonInputsData {
            core_quota,
            leader_quota,
            pow_quota,
            message_key,
            selector,
            index,
            pow_difficulty,
        }: PoQCommonInputsData,
    ) -> Result<Self, Self::Error> {
        let leader_quota_bits = leader_quota.checked_ilog2().map_or(0, |v| v + 1);
        if leader_quota_bits > 20 {
            return Err(LeaderQuotaMoreThan20Bits);
        }
        let core_quota_bits = core_quota.checked_ilog2().map_or(0, |v| v + 1);
        if core_quota_bits > 20 {
            return Err(CoreQuotaMoreThan20Bits);
        }
        let pow_quota_bits = pow_quota.checked_ilog2().map_or(0, |v| v + 1);
        if pow_quota_bits > 20 {
            return Err(PowQuotaMoreThan20Bits);
        }
        Ok(Self {
            core_quota: Groth16Input::new(Fr::from(BigUint::from(core_quota))),
            leader_quota: Groth16Input::new(Fr::from(BigUint::from(leader_quota))),
            pow_quota: Groth16Input::new(Fr::from(BigUint::from(pow_quota))),
            key_part_one: message_key.0.into(),
            key_part_two: message_key.1.into(),
            selector: Groth16Input::new(Fr::from(BigUint::from(selector as u8))),
            index: Groth16Input::new(Fr::from(BigUint::from(index))),
            pow_difficulty: pow_difficulty.into(),
        })
    }
}
