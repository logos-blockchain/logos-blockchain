pub mod kzgrs;

use std::{collections::BTreeSet, sync::Arc};

use kzgrs_backend::common::ShareIndex;
use tokio::time::Interval;

pub enum SamplingState {
    WaitingCommitments,
    Init,
    Tracking,
    Terminated,
}

#[async_trait::async_trait]
pub trait DaSamplingServiceBackend {
    type Settings;
    type BlobId;
    type Share;
    type SharesCommitments;
    fn new(settings: Self::Settings) -> Self;
    async fn get_validated_blobs(&self) -> BTreeSet<Self::BlobId>;
    async fn mark_completed(&mut self, blobs_ids: &[Self::BlobId]);
    async fn handle_sampling_success(&mut self, blob_id: Self::BlobId, column_index: ShareIndex);
    async fn handle_sampling_error(&mut self, blob_id: Self::BlobId);
    async fn init_sampling(&mut self, blob_id: Self::BlobId) -> SamplingState;
    fn prune_interval(&self) -> Interval;
    fn prune(&mut self);
    fn add_commitments(&mut self, blob_id: &Self::BlobId, commitments: Self::SharesCommitments);
    fn get_commitments(&self, blob_id: &Self::BlobId) -> Option<Arc<Self::SharesCommitments>>;
}
