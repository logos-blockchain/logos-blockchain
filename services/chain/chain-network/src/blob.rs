use std::{collections::BTreeSet, fmt::Debug, marker::PhantomData, num::NonZero};

use lb_core::{
    block::Block,
    da,
    mantle::{AuthenticatedMantleTx, Op},
};
use lb_cryptarchia_engine::Slot;
use lb_da_sampling_service::DaSamplingServiceMsg;
use lb_time_service::TimeServiceMessage;
use tokio::sync::oneshot;
use tracing::debug;

use crate::{LOG_TARGET, SamplingRelay, relays::TimeRelay};

/// An instance for validating blobs in blocks.
#[derive(Clone)]
pub struct Validation<S: Strategy> {
    consensus_base_period_length: NonZero<u64>,
    sampling_relay: SamplingRelay<da::BlobId>,
    time_relay: TimeRelay,
    _phantom: PhantomData<S>,
}

impl<S: Strategy> Validation<S> {
    pub const fn new(
        consensus_base_period_length: NonZero<u64>,
        sampling_relay: SamplingRelay<da::BlobId>,
        time_relay: TimeRelay,
    ) -> Self {
        Self {
            consensus_base_period_length,
            sampling_relay,
            time_relay,
            _phantom: PhantomData,
        }
    }
}

impl<S: Strategy + Sync> Validation<S> {
    /// Validate blobs in the given block.
    ///
    /// If the block is outside the blob validation window, which is calculated
    /// based on the current slot and the consensus base period length,
    /// no validation is performed and `Ok(())` is returned.
    pub async fn validate<Tx>(&self, block: &Block<Tx>) -> Result<(), Error>
    where
        Tx: AuthenticatedMantleTx + Sync,
    {
        if !should_validate_blobs(
            block.header().slot(),
            get_current_slot(&self.time_relay).await?,
            self.consensus_base_period_length,
        ) {
            return Ok(());
        }

        S::validate(block, &self.sampling_relay).await
    }
}

async fn get_current_slot(time_relay: &TimeRelay) -> Result<Slot, Error> {
    let (sender, receiver) = oneshot::channel();
    time_relay
        .send(TimeServiceMessage::CurrentSlot { sender })
        .await
        .map_err(|(e, _)| e)?;
    Ok(receiver.await?.slot)
}

fn should_validate_blobs(
    block_slot: Slot,
    current_slot: Slot,
    consensus_base_period_length: NonZero<u64>,
) -> bool {
    current_slot.saturating_sub(block_slot)
        <= blob_validation_window_in_slots(consensus_base_period_length)
}

const fn blob_validation_window_in_slots(consensus_base_period_length: NonZero<u64>) -> Slot {
    Slot::new(consensus_base_period_length.get() / 2)
}

#[async_trait::async_trait]
pub trait Strategy {
    async fn validate<Tx>(
        block: &Block<Tx>,
        sampling_relay: &SamplingRelay<da::BlobId>,
    ) -> Result<(), Error>
    where
        Tx: AuthenticatedMantleTx + Sync;
}

/// Validation strategy for blobs in blocks received through recent block
/// propagation, under the assumption that the DA sampling service has already
/// sampled and validated the blobs.
pub struct RecentBlobStrategy;

#[async_trait::async_trait]
impl Strategy for RecentBlobStrategy {
    async fn validate<Tx>(
        block: &Block<Tx>,
        sampling_relay: &SamplingRelay<da::BlobId>,
    ) -> Result<(), Error>
    where
        Tx: AuthenticatedMantleTx + Sync,
    {
        debug!(target = LOG_TARGET, "Validating recent blobs");
        let sampled_blobs = get_sampled_blobs(sampling_relay).await?;
        let all_blobs_sampled = block
            .transactions()
            .flat_map(|tx| tx.mantle_tx().ops.iter())
            .filter_map(|op| {
                if let Op::ChannelBlob(op) = op {
                    Some(op.blob)
                } else {
                    None
                }
            })
            .all(|blob| sampled_blobs.contains(&blob));
        if all_blobs_sampled {
            Ok(())
        } else {
            Err(Error::InvalidBlobs)
        }
    }
}

/// Validation strategy for blobs in blocks retrieved manually (e.g. chain
/// bootstrapping or orphan handling), under the assumption that the DA sampling
/// service has not yet sampled and validated the blobs.
#[derive(Clone)]
pub struct HistoricBlobStrategy;

#[async_trait::async_trait]
impl Strategy for HistoricBlobStrategy {
    async fn validate<Tx>(
        block: &Block<Tx>,
        sampling_relay: &SamplingRelay<da::BlobId>,
    ) -> Result<(), Error>
    where
        Tx: AuthenticatedMantleTx + Sync,
    {
        debug!(target = LOG_TARGET, "Validating historic blobs");

        let (sender, receiver) = oneshot::channel();
        sampling_relay
            .send(DaSamplingServiceMsg::RequestHistoricSampling {
                block_id: block.header().id(),
                blob_ids: block
                    .transactions()
                    .flat_map(|tx| tx.mantle_tx().ops.iter())
                    .filter_map(|op| {
                        if let Op::ChannelBlob(op) = op {
                            Some((op.blob, op.session))
                        } else {
                            None
                        }
                    })
                    .collect(),
                reply_channel: sender,
            })
            .await
            .map_err(|(e, _)| e)?;

        let sampling_succeeded = receiver.await?;
        if sampling_succeeded {
            Ok(())
        } else {
            Err(Error::InvalidBlobs)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Block contains invalid blobs")]
    InvalidBlobs,
    #[error("Relay error: {0}")]
    Relay(#[from] overwatch::services::relay::RelayError),
    #[error("Reply channel error: {0}")]
    ReplyRecv(#[from] oneshot::error::RecvError),
}

/// Retrieves all the blobs that have been sampled by the sampling service.
pub async fn get_sampled_blobs<BlobId>(
    sampling_relay: &SamplingRelay<BlobId>,
) -> Result<BTreeSet<BlobId>, Error>
where
    BlobId: Send,
{
    let (sender, receiver) = oneshot::channel();
    sampling_relay
        .send(DaSamplingServiceMsg::GetValidatedBlobs {
            reply_channel: sender,
        })
        .await
        .map_err(|(e, _)| e)?;
    Ok(receiver.await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blob_validation_window_in_slots() {
        assert_eq!(
            blob_validation_window_in_slots(10.try_into().unwrap()),
            5.into()
        );
        assert_eq!(
            blob_validation_window_in_slots(7.try_into().unwrap()),
            3.into() // Should round down
        );
        assert_eq!(
            blob_validation_window_in_slots(1.try_into().unwrap()),
            0.into() // Should round down
        );
    }

    #[test]
    fn test_should_validate_blobs() {
        // (103 - 100) <= (10 / 2)
        assert!(should_validate_blobs(
            100.into(),
            103.into(),
            10.try_into().unwrap()
        ));
        // (105 - 100) <= (10 / 2)
        assert!(should_validate_blobs(
            100.into(),
            105.into(),
            10.try_into().unwrap()
        ));
        // (106 - 100) > (10 / 2)
        assert!(!should_validate_blobs(
            100.into(),
            106.into(),
            10.try_into().unwrap()
        ));
        // saturating(100 - 101) <= (10 / 2)
        assert!(should_validate_blobs(
            101.into(),
            100.into(),
            10.try_into().unwrap()
        ));
    }
}
