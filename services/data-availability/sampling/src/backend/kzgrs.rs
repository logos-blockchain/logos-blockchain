use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt::Debug,
    sync::Arc,
};

use lb_core::da::BlobId;
use lb_da_network_core::SubnetworkId;
use lb_kzgrs_backend::common::{
    ShareIndex,
    share::{DaShare, DaSharesCommitments},
};
use lb_tracing::info_with_id;
use serde::{Deserialize, Serialize};
use tokio::{
    time,
    time::{Duration, Instant, Interval},
};
use tracing::instrument;

use crate::{DaSamplingServiceBackend, backend::SamplingState};

#[derive(Clone)]
pub struct SamplingContext {
    subnets: HashSet<SubnetworkId>,
    started: Instant,
    commitment: Option<Arc<DaSharesCommitments>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KzgrsSamplingBackendSettings {
    pub num_samples: u16,
    pub num_subnets: u16,
    pub old_blobs_check_interval: Duration,
    pub blobs_validity_duration: Duration,
}

pub struct KzgrsSamplingBackend {
    settings: KzgrsSamplingBackendSettings,
    validated_blobs: BTreeSet<BlobId>,
    pending_sampling_blobs: HashMap<BlobId, SamplingContext>,
}

impl KzgrsSamplingBackend {
    fn prune_by_time(&mut self) {
        self.pending_sampling_blobs.retain(|_blob_id, context| {
            context.started.elapsed() < self.settings.blobs_validity_duration
        });
    }
}

#[async_trait::async_trait]
impl DaSamplingServiceBackend for KzgrsSamplingBackend {
    type Settings = KzgrsSamplingBackendSettings;
    type BlobId = BlobId;
    type Share = DaShare;
    type SharesCommitments = DaSharesCommitments;

    fn new(settings: Self::Settings) -> Self {
        let bt: BTreeSet<BlobId> = BTreeSet::new();
        Self {
            settings,
            validated_blobs: bt,
            pending_sampling_blobs: HashMap::new(),
        }
    }

    fn prune_interval(&self) -> Interval {
        time::interval(self.settings.old_blobs_check_interval)
    }

    async fn get_validated_blobs(&self) -> BTreeSet<Self::BlobId> {
        self.validated_blobs.clone()
    }

    #[instrument(skip_all)]
    async fn mark_completed(&mut self, blobs_ids: &[Self::BlobId]) {
        for id in blobs_ids {
            info_with_id!(id, "MarkInBlock");
            self.pending_sampling_blobs.remove(id);
            self.validated_blobs.remove(id);
        }
    }

    async fn handle_sampling_success(&mut self, blob_id: Self::BlobId, column_idx: ShareIndex) {
        if let Some(ctx) = self.pending_sampling_blobs.get_mut(&blob_id) {
            tracing::info!(
                "subnet {} for blob id {} has been successfully sampled",
                column_idx,
                hex::encode(blob_id)
            );
            ctx.subnets.insert(column_idx);

            // sampling of this blob_id terminated successfully
            if ctx.subnets.len() == self.settings.num_samples as usize {
                self.validated_blobs.insert(blob_id);
                tracing::info!(
                    "blob_id {} has been successfully sampled",
                    hex::encode(blob_id)
                );
                // cleanup from pending samplings
                self.pending_sampling_blobs.remove(&blob_id);
            }
        }
    }

    async fn handle_sampling_error(&mut self, blob_id: Self::BlobId) {
        // If it fails a single time we consider it failed.
        // We may want to abstract the sampling policies somewhere else at some point if
        // we need to get fancier than this
        //
        // Sampling service subscribes to the DaNetwork to send and receive messages,
        // other services could be sending sampling requests for their use (like
        // dispersal service).
        //
        // Only act on errors for the blobs that requested through this service.
        if self.pending_sampling_blobs.remove(&blob_id).is_some() {
            self.validated_blobs.remove(&blob_id);
        }
    }

    async fn init_sampling(&mut self, blob_id: Self::BlobId) -> SamplingState {
        if self.pending_sampling_blobs.contains_key(&blob_id) {
            return SamplingState::Tracking;
        }
        if self.validated_blobs.contains(&blob_id) {
            return SamplingState::Terminated;
        }

        let ctx: SamplingContext = SamplingContext {
            subnets: HashSet::new(),
            started: Instant::now(),
            commitment: None,
        };
        self.pending_sampling_blobs.insert(blob_id, ctx);
        SamplingState::Init
    }

    fn prune(&mut self) {
        self.prune_by_time();
    }

    fn get_commitments(&self, blob_id: &Self::BlobId) -> Option<Arc<Self::SharesCommitments>> {
        let ctx = self.pending_sampling_blobs.get(blob_id)?;
        ctx.commitment.clone()
    }

    fn add_commitments(&mut self, blob_id: &Self::BlobId, commitments: Self::SharesCommitments) {
        if let Some(ctx) = self.pending_sampling_blobs.get_mut(blob_id) {
            ctx.commitment = Some(Arc::new(commitments));
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use lb_core::da::BlobId;
    use lb_kzgrs::Proof;
    use lb_kzgrs_backend::common::{Column, share::DaShare};
    use rand::prelude::*;
    use tokio::time::Duration;

    use crate::backend::kzgrs::{
        DaSamplingServiceBackend as _, KzgrsSamplingBackend, KzgrsSamplingBackendSettings,
        SamplingContext, SamplingState,
    };

    fn create_sampler(num_samples: usize, num_subnets: usize) -> KzgrsSamplingBackend {
        let settings = KzgrsSamplingBackendSettings {
            num_samples: num_samples as u16,
            num_subnets: num_subnets as u16,
            old_blobs_check_interval: Duration::from_millis(20),
            blobs_validity_duration: Duration::from_millis(10),
        };
        KzgrsSamplingBackend::new(settings)
    }

    #[tokio::test]
    async fn test_sampler() {
        // fictitious number of subnets
        let subnet_num: usize = 42;

        // create a sampler instance
        let sampler = &mut create_sampler(subnet_num, 42);
        let mut rng = StdRng::from_entropy();

        // create some blobs and blob_ids
        let b1: BlobId = rng.r#gen();
        let b2: BlobId = rng.r#gen();
        let share = DaShare {
            share_idx: 42,
            column: Column(vec![]),
            combined_column_proof: Proof::default(),
            rows_commitments: vec![],
        };
        let share2 = share.clone();
        let mut share3 = share2.clone();

        // at start everything should be empty
        assert!(sampler.pending_sampling_blobs.is_empty());
        assert!(sampler.validated_blobs.is_empty());
        assert!(sampler.get_validated_blobs().await.is_empty());

        // start sampling for b1
        let SamplingState::Init = sampler.init_sampling(b1).await else {
            panic!("unexpected return value")
        };
        assert!(sampler.validated_blobs.is_empty());
        assert!(sampler.pending_sampling_blobs.len() == 1);

        // start sampling for b2
        let SamplingState::Init = sampler.init_sampling(b2).await else {
            panic!("unexpected return value")
        };
        assert!(sampler.validated_blobs.is_empty());
        assert!(sampler.pending_sampling_blobs.len() == 2);

        // mark in block for both
        // collections should be reset
        sampler.mark_completed(&[b1, b2]).await;
        assert!(sampler.pending_sampling_blobs.is_empty());
        assert!(sampler.validated_blobs.is_empty());

        // because they're reset, we need to restart sampling for the test
        _ = sampler.init_sampling(b1).await;
        _ = sampler.init_sampling(b2).await;

        // handle ficticious error for b2
        // b2 should be gone, b1 still around
        sampler.handle_sampling_error(b2).await;
        assert!(sampler.validated_blobs.is_empty());
        assert!(sampler.pending_sampling_blobs.len() == 1);
        assert!(sampler.pending_sampling_blobs.contains_key(&b1));

        // handle ficticious sampling success for b1
        // should still just have one pending blob, no validated blobs yet,
        // and one subnet added to blob
        sampler.handle_sampling_success(b1, share.share_idx).await;
        assert!(sampler.validated_blobs.is_empty());
        assert!(sampler.pending_sampling_blobs.len() == 1);
        println!(
            "{}",
            sampler
                .pending_sampling_blobs
                .get(&b1)
                .unwrap()
                .subnets
                .len()
        );
        assert!(
            sampler
                .pending_sampling_blobs
                .get(&b1)
                .unwrap()
                .subnets
                .len()
                == 1
        );

        // handle_success for always the same subnet
        // by adding number of subnets time the same subnet
        // (column_idx did not change)
        // should not change, still just one sampling blob,
        // no validated blobs and one subnet
        for _ in 1..subnet_num {
            let b = share2.clone();
            sampler.handle_sampling_success(b1, b.share_idx).await;
        }
        assert!(sampler.validated_blobs.is_empty());
        assert!(sampler.pending_sampling_blobs.len() == 1);
        assert!(
            sampler
                .pending_sampling_blobs
                .get(&b1)
                .unwrap()
                .subnets
                .len()
                == 1
        );

        // handle_success for up to subnet size minus one subnet
        // should still not change anything
        // but subnets len is now subnet size minus one
        // we already added subnet 42
        for i in 1..(subnet_num - 1) {
            let mut b = share2.clone();
            b.share_idx = i as u16;
            sampler.handle_sampling_success(b1, b.share_idx).await;
        }
        assert!(sampler.validated_blobs.is_empty());
        assert!(sampler.pending_sampling_blobs.len() == 1);
        assert!(
            sampler
                .pending_sampling_blobs
                .get(&b1)
                .unwrap()
                .subnets
                .len()
                == subnet_num - 1
        );

        // now add the last subnet!
        // we should have all subnets set,
        // and the validated blobs should now have that blob
        // pending blobs should now be empty
        share3.share_idx = (subnet_num - 1) as u16;
        sampler.handle_sampling_success(b1, share3.share_idx).await;
        assert!(sampler.pending_sampling_blobs.is_empty());
        assert!(sampler.validated_blobs.len() == 1);
        assert!(sampler.validated_blobs.contains(&b1));
        // these checks are redundant but better safe than sorry
        assert!(sampler.get_validated_blobs().await.len() == 1);
        assert!(sampler.get_validated_blobs().await.contains(&b1));

        // run mark_in_block for the same blob
        // should return empty for everything
        sampler.mark_completed(&[b1]).await;
        assert!(sampler.validated_blobs.is_empty());
        assert!(sampler.pending_sampling_blobs.is_empty());
    }

    #[tokio::test]
    async fn test_pruning() {
        tokio::time::pause();

        let mut sampler = create_sampler(42, 42);

        // create expired contexts first (before advancing time)
        let ctx11 = SamplingContext {
            subnets: HashSet::new(),
            started: tokio::time::Instant::now(),
            commitment: None,
        };
        let ctx12 = ctx11.clone();
        let ctx13 = ctx11.clone();

        // advance time by 2 seconds to make the above contexts expired
        tokio::time::advance(Duration::from_secs(2)).await;

        // create fresh contexts after advancing time
        let ctx1 = SamplingContext {
            subnets: HashSet::new(),
            started: tokio::time::Instant::now(),
            commitment: None,
        };
        let ctx2 = ctx1.clone();
        let ctx3 = ctx1.clone();

        let mut rng = StdRng::from_entropy();
        // create a couple blob ids
        let b1: BlobId = rng.r#gen();
        let b2: BlobId = rng.r#gen();
        let b3: BlobId = rng.r#gen();

        // insert first blob
        // pruning should have no effect
        assert!(sampler.pending_sampling_blobs.is_empty());
        sampler.pending_sampling_blobs.insert(b1, ctx1);
        sampler.prune();
        assert!(sampler.pending_sampling_blobs.len() == 1);

        // insert second blob
        // pruning should have no effect
        sampler.pending_sampling_blobs.insert(b2, ctx2);
        sampler.prune();
        assert!(sampler.pending_sampling_blobs.len() == 2);

        // insert third blob
        // pruning should have no effect
        sampler.pending_sampling_blobs.insert(b3, ctx3);
        sampler.prune();
        assert!(sampler.pending_sampling_blobs.len() == 3);

        // insert fake expired blobs
        // pruning these should now decrease pending blobx every time
        sampler.pending_sampling_blobs.insert(b1, ctx11);
        sampler.prune();
        assert!(sampler.pending_sampling_blobs.len() == 2);

        sampler.pending_sampling_blobs.insert(b2, ctx12);
        sampler.prune();
        assert!(sampler.pending_sampling_blobs.len() == 1);

        sampler.pending_sampling_blobs.insert(b3, ctx13);
        sampler.prune();
        assert!(sampler.pending_sampling_blobs.is_empty());
    }
}
