use lb_core::da::BlobId;
use lb_kzgrs_backend::common::share::{DaLightShare, DaSharesCommitments};
use lb_subnetworks_assignations::SubnetworkId;

use crate::protocols::sampling::{errors::SamplingError, opinions::OpinionEvent};

pub mod request_behaviour;

#[derive(Debug)]
pub enum SamplingEvent {
    /// A blob successfully arrived its destination
    SamplingSuccess {
        blob_id: BlobId,
        subnetwork_id: SubnetworkId,
        light_share: Box<DaLightShare>,
    },
    CommitmentsSuccess {
        blob_id: BlobId,
        commitments: Box<DaSharesCommitments>,
    },
    SamplingError {
        error: SamplingError,
    },
    Opinion(OpinionEvent),
}
