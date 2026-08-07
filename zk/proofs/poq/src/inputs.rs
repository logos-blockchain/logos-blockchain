use lb_groth16::{AdditiveGroup as _, Fr, Groth16Input, Groth16InputDeser};
use serde::{Deserialize, Serialize};

use crate::{
    PoQChainInputsData, PoQCommonInputsData, Quota,
    blend_inputs::{PoQBlendInputs, PoQBlendInputsData, PoQBlendInputsJson},
    chain_inputs::{PoQChainInputs, PoQChainInputsJson},
    common_inputs::{PoQCommonInputs, PoQCommonInputsJson},
    pow_inputs::{PoQPowInputs, PoQPowInputsData, PoQPowInputsJson},
    wallet_inputs::{PoQWalletInputs, PoQWalletInputsData, PoQWalletInputsJson},
};

#[derive(Clone, Serialize)]
#[serde(into = "PoQInputsJson", rename_all = "snake_case")]
pub struct PoQWitnessInputs {
    pub chain: PoQChainInputs,
    pub common: PoQCommonInputs,
    pub blend: PoQBlendInputs,
    pub wallet: PoQWalletInputs,
    pub pow: PoQPowInputs,
}

impl PoQWitnessInputs {
    #[must_use]
    pub fn from_leader_data(
        chain: PoQChainInputsData,
        common: PoQCommonInputsData,
        wallet: PoQWalletInputsData,
    ) -> Self {
        Self {
            chain: chain.into(),
            common: common.into(),
            blend: Self::unused_blend_inputs(),
            wallet: wallet.into(),
            pow: Self::unused_pow_inputs(),
        }
    }

    #[must_use]
    pub fn from_core_node_data(
        chain: PoQChainInputsData,
        common: PoQCommonInputsData,
        blend: PoQBlendInputsData,
    ) -> Self {
        Self {
            chain: chain.into(),
            common: common.into(),
            blend: blend.into(),
            wallet: Self::unused_wallet_inputs(),
            pow: Self::unused_pow_inputs(),
        }
    }

    #[must_use]
    pub fn from_pow_data(
        chain: PoQChainInputsData,
        common: PoQCommonInputsData,
        pow: PoQPowInputsData,
    ) -> Self {
        Self {
            chain: chain.into(),
            common: common.into(),
            blend: Self::unused_blend_inputs(),
            wallet: Self::unused_wallet_inputs(),
            pow: pow.into(),
        }
    }

    fn unused_blend_inputs() -> PoQBlendInputs {
        PoQBlendInputs::from(PoQBlendInputsData {
            core_sk: Fr::ZERO,
            core_path_and_selectors: [(Fr::ZERO, false); _],
        })
    }

    fn unused_wallet_inputs() -> PoQWalletInputs {
        PoQWalletInputs::from(PoQWalletInputsData {
            slot: 0,
            note_value: 0,
            transaction_hash: Fr::ZERO,
            output_number: 0,
            aged_path_and_selectors: [(Fr::ZERO, false); _],
            pol_secret_key: Fr::ZERO,
        })
    }

    fn unused_pow_inputs() -> PoQPowInputs {
        PoQPowInputs::from(PoQPowInputsData {
            pow_secret_key: Fr::ZERO,
            block_hash: Fr::ZERO,
        })
    }
}

impl TryFrom<PoQWitnessInputs> for lbc_poq_sys::PoqWitnessInput<'_> {
    type Error = lbp_error::Error;

    fn try_from(value: PoQWitnessInputs) -> Result<Self, Self::Error> {
        let inputs_json: PoQInputsJson = value.into();
        let inputs_str: String = serde_json::to_string(&inputs_json)?;
        let witness_input = lbc_poq_sys::PoqWitnessInput::new(inputs_str)?;
        Ok(witness_input)
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
    #[serde(flatten)]
    pub pow: PoQPowInputsJson,
}

impl From<PoQWitnessInputs> for PoQInputsJson {
    fn from(inputs: PoQWitnessInputs) -> Self {
        Self {
            wallet: inputs.wallet.into(),
            chain: (&inputs.chain).into(),
            common: (&inputs.common).into(),
            blend: inputs.blend.into(),
            pow: inputs.pow.into(),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct PoQVerifierInputJson([Groth16InputDeser; 12]);

#[derive(Clone)]
pub struct PoQVerifierInput {
    pub key_nullifier: Groth16Input,
    pub core_quota: Groth16Input,
    pub leader_quota: Groth16Input,
    pub core_root: Groth16Input,
    pub pow_quota: Groth16Input,
    pub k_part_one: Groth16Input,
    pub k_part_two: Groth16Input,
    pub pow_blend_difficulty: Groth16Input,
    pub pol_epoch_nonce: Groth16Input,
    pub pol_t0: Groth16Input,
    pub pol_t1: Groth16Input,
    pub pol_ledger_aged: Groth16Input,
}

pub struct PoQVerifierInputData {
    pub key_nullifier: Fr,
    pub core_quota: Quota,
    pub leader_quota: Quota,
    pub core_root: Fr,
    pub pow_quota: Quota,
    pub k_part_one: Fr,
    pub k_part_two: Fr,
    pub pow_blend_difficulty: Fr,
    pub pol_epoch_nonce: Fr,
    pub lottery_0: Fr,
    pub lottery_1: Fr,
    pub pol_ledger_aged: Fr,
}

impl TryFrom<PoQVerifierInputJson> for PoQVerifierInput {
    type Error = <Groth16Input as TryFrom<Groth16InputDeser>>::Error;

    fn try_from(value: PoQVerifierInputJson) -> Result<Self, Self::Error> {
        let [
            key_nullifier,
            core_quota,
            leader_quota,
            core_root,
            pow_quota,
            pol_ledger_aged,
            k_part_one,
            k_part_two,
            pow_blend_difficulty,
            pol_epoch_nonce,
            pol_t0,
            pol_t1,
        ] = value.0;
        Ok(Self {
            key_nullifier: key_nullifier.try_into()?,
            core_quota: core_quota.try_into()?,
            leader_quota: leader_quota.try_into()?,
            core_root: core_root.try_into()?,
            pow_quota: pow_quota.try_into()?,
            k_part_one: k_part_one.try_into()?,
            k_part_two: k_part_two.try_into()?,
            pow_blend_difficulty: pow_blend_difficulty.try_into()?,
            pol_epoch_nonce: pol_epoch_nonce.try_into()?,
            pol_t0: pol_t0.try_into()?,
            pol_t1: pol_t1.try_into()?,
            pol_ledger_aged: pol_ledger_aged.try_into()?,
        })
    }
}

impl PoQVerifierInput {
    #[must_use]
    pub const fn to_inputs(self) -> [Fr; 12] {
        [
            self.key_nullifier.into_inner(),
            self.core_quota.into_inner(),
            self.leader_quota.into_inner(),
            self.core_root.into_inner(),
            self.pow_quota.into_inner(),
            self.pol_ledger_aged.into_inner(),
            self.k_part_one.into_inner(),
            self.k_part_two.into_inner(),
            self.pow_blend_difficulty.into_inner(),
            self.pol_epoch_nonce.into_inner(),
            self.pol_t0.into_inner(),
            self.pol_t1.into_inner(),
        ]
    }
}

impl From<PoQVerifierInputData> for PoQVerifierInput {
    fn from(value: PoQVerifierInputData) -> Self {
        Self {
            core_quota: value.core_quota.into(),
            core_root: value.core_root.into(),
            k_part_one: value.k_part_one.into(),
            k_part_two: value.k_part_two.into(),
            key_nullifier: value.key_nullifier.into(),
            leader_quota: value.leader_quota.into(),
            pow_quota: value.pow_quota.into(),
            pow_blend_difficulty: value.pow_blend_difficulty.into(),
            pol_epoch_nonce: value.pol_epoch_nonce.into(),
            pol_ledger_aged: value.pol_ledger_aged.into(),
            pol_t0: Groth16Input::new(value.lottery_0),
            pol_t1: Groth16Input::new(value.lottery_1),
        }
    }
}
