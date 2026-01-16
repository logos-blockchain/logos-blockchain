use crate::ZkHash;

/// Set of inputs required to verify a Proof of Selection.
#[derive(Debug, Clone, Copy)]
pub struct VerifyInputs {
    pub expected_node_index: u64,
    pub total_membership_size: u64,
    pub key_nullifier: ZkHash,
}
