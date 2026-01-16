use groth16::{Field as _, Fr, Groth16Input, Groth16InputDeser};
use pol::compute_lottery_values;
use serde::{Deserialize, Serialize};

use crate::{
    PoQChainInputsData, PoQCommonInputsData,
    blend_inputs::{PoQBlendInputs, PoQBlendInputsData, PoQBlendInputsJson},
    chain_inputs::{PoQChainInputs, PoQChainInputsJson},
    common_inputs::{PoQCommonInputs, PoQCommonInputsJson},
    wallet_inputs::{PoQWalletInputs, PoQWalletInputsData, PoQWalletInputsJson},
};

#[derive(Clone, Serialize)]
#[serde(into = "PoQInputsJson", rename_all = "snake_case")]
pub struct PoQWitnessInputs {
    pub chain: PoQChainInputs,
    pub common: PoQCommonInputs,
    pub blend: PoQBlendInputs,
    pub wallet: PoQWalletInputs,
}

impl PoQWitnessInputs {
    pub fn from_leader_data(
        chain: PoQChainInputsData,
        common: PoQCommonInputsData,
        wallet: PoQWalletInputsData,
    ) -> Result<Self, <PoQChainInputs as TryFrom<PoQChainInputsData>>::Error> {
        Ok(Self {
            chain: chain.try_into()?,
            common: common.try_into()?,
            blend: PoQBlendInputs::from(PoQBlendInputsData {
                core_sk: Fr::ZERO,
                core_path_and_selectors: [(Fr::ZERO, false); _],
            }),
            wallet: wallet.into(),
        })
    }

    pub fn from_core_node_data(
        chain: PoQChainInputsData,
        common: PoQCommonInputsData,
        blend: PoQBlendInputsData,
    ) -> Result<Self, <PoQChainInputs as TryFrom<PoQChainInputsData>>::Error> {
        Ok(Self {
            chain: chain.try_into()?,
            common: common.try_into()?,
            blend: blend.into(),
            wallet: PoQWalletInputs::from(PoQWalletInputsData {
                slot: 0,
                note_value: 0,
                transaction_hash: Fr::ZERO,
                output_number: 0,
                aged_path_and_selectors: [(Fr::ZERO, false); _],
                secret_key: Fr::ZERO,
            }),
        })
    }
}

#[derive(Serialize)]
pub struct PoQInputsJson {
    #[serde(flatten)]
    pub chain: PoQChainInputsJson,
    #[serde(flatten)]
    pub common: PoQCommonInputsJson,
    #[serde(flatten)]
    pub blend: PoQBlendInputsJson,
    #[serde(flatten)]
    pub wallet: PoQWalletInputsJson,
}

impl From<PoQWitnessInputs> for PoQInputsJson {
    fn from(inputs: PoQWitnessInputs) -> Self {
        Self {
            wallet: inputs.wallet.into(),
            chain: (&inputs.chain).into(),
            common: (&inputs.common).into(),
            blend: inputs.blend.into(),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct PoQVerifierInputJson([Groth16InputDeser; 11]);

pub struct PoQVerifierInput {
    pub key_nullifier: Groth16Input,
    pub session: Groth16Input,
    pub core_quota: Groth16Input,
    pub leader_quota: Groth16Input,
    pub core_root: Groth16Input,
    pub k_part_one: Groth16Input,
    pub k_part_two: Groth16Input,
    pub pol_epoch_nonce: Groth16Input,
    pub pol_t0: Groth16Input,
    pub pol_t1: Groth16Input,
    pub pol_ledger_aged: Groth16Input,
}

pub struct PoQVerifierInputData {
    pub key_nullifier: Fr,
    pub session: u64,
    pub core_quota: u64,
    pub leader_quota: u64,
    pub core_root: Fr,
    pub k_part_one: Fr,
    pub k_part_two: Fr,
    pub pol_epoch_nonce: Fr,
    pub total_stake: u64,
    pub pol_ledger_aged: Fr,
}

impl TryFrom<PoQVerifierInputJson> for PoQVerifierInput {
    type Error = <Groth16Input as TryFrom<Groth16InputDeser>>::Error;

    fn try_from(value: PoQVerifierInputJson) -> Result<Self, Self::Error> {
        let [
            key_nullifier,
            session,
            core_quota,
            leader_quota,
            core_root,
            pol_ledger_aged,
            k_part_one,
            k_part_two,
            pol_epoch_nonce,
            pol_t0,
            pol_t1,
        ] = value.0;
        Ok(Self {
            key_nullifier: key_nullifier.try_into()?,
            session: session.try_into()?,
            core_quota: core_quota.try_into()?,
            leader_quota: leader_quota.try_into()?,
            core_root: core_root.try_into()?,
            k_part_one: k_part_one.try_into()?,
            k_part_two: k_part_two.try_into()?,
            pol_epoch_nonce: pol_epoch_nonce.try_into()?,
            pol_t0: pol_t0.try_into()?,
            pol_t1: pol_t1.try_into()?,
            pol_ledger_aged: pol_ledger_aged.try_into()?,
        })
    }
}

impl PoQVerifierInput {
    #[must_use]
    pub const fn to_inputs(self) -> [Fr; 11] {
        [
            self.key_nullifier.into_inner(),
            self.session.into_inner(),
            self.core_quota.into_inner(),
            self.leader_quota.into_inner(),
            self.core_root.into_inner(),
            self.pol_ledger_aged.into_inner(),
            self.k_part_one.into_inner(),
            self.k_part_two.into_inner(),
            self.pol_epoch_nonce.into_inner(),
            self.pol_t0.into_inner(),
            self.pol_t1.into_inner(),
        ]
    }
}

impl From<PoQVerifierInputData> for PoQVerifierInput {
    fn from(value: PoQVerifierInputData) -> Self {
        let (lottery_0, lottery_1) = compute_lottery_values(value.total_stake);

        Self {
            core_quota: Groth16Input::new(value.core_quota.into()),
            core_root: value.core_root.into(),
            k_part_one: value.k_part_one.into(),
            k_part_two: value.k_part_two.into(),
            key_nullifier: value.key_nullifier.into(),
            leader_quota: Groth16Input::new(value.leader_quota.into()),
            pol_epoch_nonce: value.pol_epoch_nonce.into(),
            pol_ledger_aged: value.pol_ledger_aged.into(),
            pol_t0: Groth16Input::new(lottery_0.into()),
            pol_t1: Groth16Input::new(lottery_1.into()),
            session: Groth16Input::new(value.session.into()),
        }
    }
}
