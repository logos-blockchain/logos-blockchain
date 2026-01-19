use std::collections::HashMap;

use lb_core::{da::BlobId, header::HeaderId};
use lb_kzgrs_backend::common::share::{DaLightShare, DaSharesCommitments};

use crate::protocols::sampling::{
    errors::{HistoricCommitmentsError, HistoricSamplingError},
    opinions::OpinionEvent,
};

pub mod request_behaviour;

#[derive(Debug)]
pub enum HistoricSamplingEvent {
    SamplingSuccess {
        block_id: HeaderId,
        commitments: HashMap<BlobId, DaSharesCommitments>,
        shares: HashMap<BlobId, Vec<DaLightShare>>,
    },
    SamplingError {
        block_id: HeaderId,
        error: HistoricSamplingError,
    },
    CommitmentsSuccess {
        block_id: HeaderId,
        blob_id: BlobId,
        commitments: DaSharesCommitments,
    },
    CommitmentsError {
        block_id: HeaderId,
        error: HistoricCommitmentsError,
    },
    Opinion(OpinionEvent),
}
