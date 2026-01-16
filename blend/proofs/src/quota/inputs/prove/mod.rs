use groth16::fr_from_bytes;
use poq::{
    PoQBlendInputsData, PoQChainInputsData, PoQCommonInputsData, PoQInputsFromDataError,
    PoQWalletInputsData, PoQWitnessInputs,
};

use crate::quota::inputs::{
    prove::private::{ProofOfCoreQuotaInputs, ProofOfLeadershipQuotaInputs, ProofType},
    split_ephemeral_signing_key,
};

pub mod private;
pub mod public;
pub use self::{private::Inputs as PrivateInputs, public::Inputs as PublicInputs};

#[derive(Debug, Clone)]
pub(crate) struct Inputs {
    pub public: PublicInputs,
    pub private: PrivateInputs,
}

impl TryFrom<Inputs> for PoQWitnessInputs {
    type Error = PoQInputsFromDataError;

    fn try_from(value: Inputs) -> Result<Self, Self::Error> {
        let (signing_key_first_half, signing_key_second_half) =
            split_ephemeral_signing_key(value.public.signing_key);
        let chain_input_data = PoQChainInputsData {
            core_root: value.public.core.zk_root,
            pol_epoch_nonce: value.public.leader.pol_epoch_nonce,
            pol_ledger_aged: value.public.leader.pol_ledger_aged,
            session: value.public.session,
            total_stake: value.public.leader.total_stake,
        };
        let common_input_data = PoQCommonInputsData {
            core_quota: value.public.core.quota,
            index: value.private.key_index,
            leader_quota: value.public.leader.message_quota,
            message_key: (
                fr_from_bytes(&signing_key_first_half[..]).expect(
                    "First half of signing public key does not represent a valid `Fr` point.",
                ),
                fr_from_bytes(&signing_key_second_half[..]).expect(
                    "Second half of signing public key does not represent a valid `Fr` point.",
                ),
            ),
            selector: value.private.selector,
        };
        witness_input_for_proof_type(
            chain_input_data,
            common_input_data,
            value.private.proof_type,
        )
    }
}

fn witness_input_for_proof_type(
    chain_input_data: PoQChainInputsData,
    common_input_data: PoQCommonInputsData,
    proof_type: ProofType,
) -> Result<PoQWitnessInputs, PoQInputsFromDataError> {
    match proof_type {
        ProofType::CoreQuota(core_quota_private_inputs) => {
            let ProofOfCoreQuotaInputs {
                core_path_and_selectors,
                core_sk,
            } = *core_quota_private_inputs;

            let blend_input_data = PoQBlendInputsData {
                core_path_and_selectors,
                core_sk,
            };
            PoQWitnessInputs::from_core_node_data(
                chain_input_data,
                common_input_data,
                blend_input_data,
            )
        }
        ProofType::LeadershipQuota(leadership_quota_private_inputs) => {
            let ProofOfLeadershipQuotaInputs {
                aged_path_and_selectors,
                note_value,
                output_number,
                slot,
                secret_key,
                transaction_hash,
            } = *leadership_quota_private_inputs;

            let wallet_input_data = PoQWalletInputsData {
                aged_path_and_selectors,
                note_value,
                output_number,
                slot,
                secret_key,
                transaction_hash,
            };
            PoQWitnessInputs::from_leader_data(
                chain_input_data,
                common_input_data,
                wallet_input_data,
            )
        }
    }
}
