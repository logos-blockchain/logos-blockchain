use crate::mantle::{Op, OpProof};

#[derive(Debug, thiserror::Error)]
#[error("Proof does not match {operation:?}: got {actual_proof:?}")]
pub struct OpProofMismatch {
    operation: Op,
    actual_proof: OpProof,
}

impl OpProofMismatch {
    #[must_use]
    pub const fn new(operation: Op, actual_proof: OpProof) -> Self {
        Self {
            operation,
            actual_proof,
        }
    }

    #[must_use]
    pub const fn operation(&self) -> &Op {
        &self.operation
    }

    #[must_use]
    pub const fn actual_proof(&self) -> &OpProof {
        &self.actual_proof
    }
}
