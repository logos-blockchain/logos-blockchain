use core::fmt::{self, Debug, Formatter};

use lb_poq::AgedNotePathAndSelectors;
use zeroize::ZeroizeOnDrop;

use crate::{
    CorePathAndSelectors, ZkHash,
    quota::{
        SelectionRandomnessSecretInput,
        inputs::prove::{PublicInputs, public::LeaderInputs},
    },
};

/// Private inputs for all types of Proof of Quota. Spec: <https://lip.logos.co/blockchain/raw/proof-of-quota.html#witness>.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Inputs {
    pub key_index: u64,
    pub selector: u8,
    pub proof_type: ProofType,
}

impl Inputs {
    #[must_use]
    pub fn new_proof_of_core_quota_inputs(
        key_index: u64,
        proof_of_core_quota_inputs: ProofOfCoreQuotaInputs,
    ) -> Self {
        let proof_type: ProofType = proof_of_core_quota_inputs.into();
        Self {
            key_index,
            selector: proof_type.proof_selector(),
            proof_type,
        }
    }

    #[must_use]
    pub fn new_proof_of_leadership_quota_inputs(
        key_index: u64,
        proof_of_leadership_quota_inputs: ProofOfLeadershipQuotaInputs,
    ) -> Self {
        let proof_type: ProofType = proof_of_leadership_quota_inputs.into();
        Self {
            key_index,
            selector: proof_type.proof_selector(),
            proof_type,
        }
    }

    /// Return the right `sk` for a Proof of Quota depending on the proof type, as per the spec: <https://lip.logos.co/blockchain/raw/proof-of-quota.html#constraints>.
    #[must_use]
    pub fn get_secret_selection_randomness_sk(
        &self,
        PublicInputs {
            leader: LeaderInputs {
                pol_epoch_nonce, ..
            },
            ..
        }: &PublicInputs,
    ) -> SelectionRandomnessSecretInput {
        match &self.proof_type {
            ProofType::CoreQuota(core_quota_private_inputs) => {
                SelectionRandomnessSecretInput::Core {
                    epoch_nonce: *pol_epoch_nonce,
                    sk: core_quota_private_inputs.core_sk,
                }
            }
            ProofType::LeadershipQuota(leadership_quota_private_inputs) => {
                SelectionRandomnessSecretInput::Leadership {
                    note_secret_key: leadership_quota_private_inputs.secret_key,
                    slot_number: leadership_quota_private_inputs.slot,
                }
            }
            ProofType::PowQuota(pow_quota_private_inputs) => SelectionRandomnessSecretInput::Pow {
                pow_sk: pow_quota_private_inputs.pow_sk,
                epoch_nonce: *pol_epoch_nonce,
            },
        }
    }
}

#[derive(Clone)]
pub enum ProofType {
    CoreQuota(Box<ProofOfCoreQuotaInputs>),
    LeadershipQuota(Box<ProofOfLeadershipQuotaInputs>),
    PowQuota(Box<ProofOfWorkQuotaInputs>),
}

impl Debug for ProofType {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoreQuota(_) => f.write_str("ProofType::CoreQuota"),
            Self::LeadershipQuota(_) => f.write_str("ProofType::LeadershipQuota"),
            Self::PowQuota(_) => f.write_str("ProofType::PowQuota"),
        }
    }
}

impl ProofType {
    #[must_use]
    pub const fn proof_selector(&self) -> u8 {
        match self {
            Self::CoreQuota(_) => 0,
            Self::LeadershipQuota(_) => 1,
            Self::PowQuota(_) => 2,
        }
    }
}

#[derive(Clone, PartialEq, Eq, ZeroizeOnDrop)]
pub struct ProofOfCoreQuotaInputs {
    pub core_sk: ZkHash,
    pub core_path_and_selectors: CorePathAndSelectors,
}

impl From<ProofOfCoreQuotaInputs> for ProofType {
    fn from(value: ProofOfCoreQuotaInputs) -> Self {
        Self::CoreQuota(Box::new(value))
    }
}

#[derive(Clone, PartialEq, Eq, ZeroizeOnDrop)]
pub struct ProofOfLeadershipQuotaInputs {
    pub slot: u64,
    pub note_value: u64,
    pub transaction_hash: ZkHash,
    pub output_number: u64,
    pub aged_path_and_selectors: AgedNotePathAndSelectors,
    pub secret_key: ZkHash,
}

impl From<ProofOfLeadershipQuotaInputs> for ProofType {
    fn from(value: ProofOfLeadershipQuotaInputs) -> Self {
        Self::LeadershipQuota(Box::new(value))
    }
}

#[derive(Clone, PartialEq, Eq, ZeroizeOnDrop)]
pub struct ProofOfWorkQuotaInputs {
    pub pow_sk: ZkHash,
}

impl From<ProofOfWorkQuotaInputs> for ProofType {
    fn from(value: ProofOfWorkQuotaInputs) -> Self {
        Self::PowQuota(Box::new(value))
    }
}
