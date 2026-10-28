use lb_chain_service::Epoch;
use lb_core::{
    header::HeaderId,
    mantle::{TxHash, gas::GasCost},
};
use serde::{Deserialize, Serialize};

/// The reason an activity proof submission failed
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityProofSubmissionError {
    DeclarationNotFound,
    FundingFailed,
    TransactionBuildFailed,
    MempoolRejected,
}

/// A submitted activity proof transaction
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivitySubmission {
    pub tip: HeaderId,
    pub tx_id: TxHash,
    /// The fee the transaction pays.
    pub fee: GasCost,
}

/// The outcome of a specific epoch's activity proof submission
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityProofReport {
    pub epoch: Epoch,
    pub submission: Result<ActivitySubmission, ActivityProofSubmissionError>,
}
