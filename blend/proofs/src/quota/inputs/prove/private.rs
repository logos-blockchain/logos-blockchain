use poq::NotePathAndSelectors;

use crate::{CorePathAndSelectors, ZkHash};

/// Private inputs for all types of Proof of Quota. Spec: <https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#215261aa09df81a18576f67b910d34d4>.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Inputs {
    pub key_index: u64,
    pub selector: bool,
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

    /// Return the right `sk` for a Proof of Quota depending on the proof type, as per the spec: <https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#25a261aa09df80e0a410f708190ac802>.
    #[must_use]
    pub fn get_secret_selection_randomness_sk(&self) -> ZkHash {
        match &self.proof_type {
            ProofType::CoreQuota(core_quota_private_inputs) => core_quota_private_inputs.core_sk,
            ProofType::LeadershipQuota(leadership_quota_private_inputs) => {
                leadership_quota_private_inputs.secret_key
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProofType {
    CoreQuota(Box<ProofOfCoreQuotaInputs>),
    LeadershipQuota(Box<ProofOfLeadershipQuotaInputs>),
}

impl ProofType {
    #[must_use]
    pub const fn proof_selector(&self) -> bool {
        match self {
            Self::CoreQuota(_) => false,
            Self::LeadershipQuota(_) => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofOfCoreQuotaInputs {
    pub core_sk: ZkHash,
    pub core_path_and_selectors: CorePathAndSelectors,
}

impl From<ProofOfCoreQuotaInputs> for ProofType {
    fn from(value: ProofOfCoreQuotaInputs) -> Self {
        Self::CoreQuota(Box::new(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofOfLeadershipQuotaInputs {
    pub slot: u64,
    pub note_value: u64,
    pub transaction_hash: ZkHash,
    pub output_number: u64,
    pub aged_path_and_selectors: NotePathAndSelectors,
    pub secret_key: ZkHash,
}

impl From<ProofOfLeadershipQuotaInputs> for ProofType {
    fn from(value: ProofOfLeadershipQuotaInputs) -> Self {
        Self::LeadershipQuota(Box::new(value))
    }
}
