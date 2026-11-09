//! Applies blocks one at a time and in order, on a task of its own, so that
//! however long a block takes, the service keeps taking in what peers and other
//! services send it.

use core::{fmt::Debug, pin::Pin};
use std::time::Instant;

use futures::StreamExt as _;
use lb_chain_service::api::{ApiError, CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    block::{Block, BlockLimits, Proposal},
    header::HeaderId,
    mantle::{
        ledger::verification_mode::StandardMode,
        traits::{SignedMantleTx, StorageSize},
        transactions::states::Preverified,
    },
};
use lb_cryptarchia_engine::{Era, EraSchedule};
use lb_utils::tokio::task::{CancellableHandle, spawn};
use tokio::sync::{mpsc, oneshot};
use tracing::{Level, debug, error, info, span, trace, warn};
use tracing_futures::Instrument as _;

use crate::{
    DoNotProcessBlock, Error, LOG_TARGET, SERVICE_ID, apply_block_and_reconcile_mempool,
    is_recoverable_apply_error,
    mempool::{MempoolAdapter as _, adapter::MempoolAdapter},
    metrics,
    network::NetworkAdapter,
    reconstruct_block_from_proposal, should_process_block,
    sync::{orphan_handler::OrphanBlocksDownloader, tip_poll::PolledTip},
    verify_proposal_and_cache_rejected,
};

/// How many received proposals wait for ingestion. They're dropped past this,
/// so that a flood of them can't hold anything else up. Catching up on a
/// dropped one is the same as on one that gossip never delivered.
const MAX_QUEUED_PROPOSALS: usize = 64;

/// The chain leader waits for each of its blocks, so few are ever queued.
const MAX_QUEUED_LOCAL_BLOCKS: usize = 4;

const MAX_QUEUED_POLLED_TIPS: usize = 32;

const MAX_QUEUED_ERA_CHANGES: usize = 2;

/// Where a block came from, which sets what a failed apply means for it.
#[derive(Clone, Copy)]
enum BlockSource {
    Proposal { started_at: Instant },
    OrphanDownload,
}

/// A block of this node's, and who waits to hear how applying it went.
struct LocalBlock<Tx> {
    block: Block<Tx>,
    resp: oneshot::Sender<Result<(), Error>>,
}

enum EraChange {
    Started(Era),
    TransitionEnded(Era),
}

/// The block ingestion task, as seen by the service.
pub struct IngestionHandle<Tx> {
    local_blocks: mpsc::Sender<LocalBlock<Tx>>,
    proposals: mpsc::Sender<Proposal>,
    polled_tips: mpsc::Sender<PolledTip>,
    era_changes: mpsc::Sender<EraChange>,
    task: CancellableHandle<()>,
}

impl<Tx> IngestionHandle<Tx> {
    /// Queues `proposal` for ingestion, unless too many are queued already.
    pub fn enqueue_proposal(&self, proposal: Proposal) {
        if let Err(error) = self.proposals.try_send(proposal) {
            warn!(target: LOG_TARGET, %error, "Dropping a block proposal");
            metrics::consensus_proposals_ignored_total("ingestion_queue_full", "network");
        }
    }

    pub async fn apply_local_block(
        &self,
        block: Block<Tx>,
        resp: oneshot::Sender<Result<(), Error>>,
    ) {
        if self
            .local_blocks
            .send(LocalBlock { block, resp })
            .await
            .is_err()
        {
            error!(target: LOG_TARGET, "Block ingestion is gone: dropping a local block");
        }
    }

    pub fn polled_tips(&self) -> mpsc::Sender<PolledTip> {
        self.polled_tips.clone()
    }

    pub async fn rotate_era(&self, era: Era) {
        self.report_era_change(EraChange::Started(era)).await;
    }

    pub async fn complete_era_transition(&self, ended_era: Era) {
        self.report_era_change(EraChange::TransitionEnded(ended_era))
            .await;
    }

    async fn report_era_change(&self, change: EraChange) {
        if self.era_changes.send(change).await.is_err() {
            error!(target: LOG_TARGET, "Block ingestion is gone: dropping an era change");
        }
    }

    /// Resolves if the task ends, which it does only by panicking.
    pub const fn task(&mut self) -> &mut CancellableHandle<()> {
        &mut self.task
    }
}

struct Inputs<Tx> {
    local_blocks: mpsc::Receiver<LocalBlock<Tx>>,
    proposals: mpsc::Receiver<Proposal>,
    polled_tips: mpsc::Receiver<PolledTip>,
    era_changes: mpsc::Receiver<EraChange>,
}

type Senders<Tx> = (
    mpsc::Sender<LocalBlock<Tx>>,
    mpsc::Sender<Proposal>,
    mpsc::Sender<PolledTip>,
    mpsc::Sender<EraChange>,
);

fn channels<Tx>() -> (Senders<Tx>, Inputs<Tx>) {
    let (local_blocks_tx, local_blocks) = mpsc::channel(MAX_QUEUED_LOCAL_BLOCKS);
    let (proposals_tx, proposals) = mpsc::channel(MAX_QUEUED_PROPOSALS);
    let (polled_tips_tx, polled_tips) = mpsc::channel(MAX_QUEUED_POLLED_TIPS);
    let (era_changes_tx, era_changes) = mpsc::channel(MAX_QUEUED_ERA_CHANGES);
    (
        (
            local_blocks_tx,
            proposals_tx,
            polled_tips_tx,
            era_changes_tx,
        ),
        Inputs {
            local_blocks,
            proposals,
            polled_tips,
            era_changes,
        },
    )
}

pub struct BlockIngestion<Cryptarchia, NetAdapter, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData,
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
{
    cryptarchia: CryptarchiaServiceApi<Cryptarchia>,
    mempool: MempoolAdapter<Cryptarchia::Tx>,
    orphan_downloader: Pin<Box<OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>>>,
    /// What a block of each era may hold, which a proposal of the era is
    /// rebuilt under.
    block_limits: EraSchedule<BlockLimits>,
}

impl<Cryptarchia, NetAdapter, RuntimeServiceId>
    BlockIngestion<Cryptarchia, NetAdapter, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Send
        + Sync
        + 'static,
    NetAdapter: NetworkAdapter<RuntimeServiceId, Block = Block<Cryptarchia::Tx>>
        + Clone
        + Send
        + Sync
        + 'static,
    RuntimeServiceId: Send + Sync + 'static,
{
    pub fn new(
        cryptarchia: CryptarchiaServiceApi<Cryptarchia>,
        mempool: MempoolAdapter<Cryptarchia::Tx>,
        orphan_downloader: OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>,
        block_limits: EraSchedule<BlockLimits>,
    ) -> Self {
        Self {
            cryptarchia,
            mempool,
            orphan_downloader: Box::pin(orphan_downloader),
            block_limits,
        }
    }

    /// Starts the task. It stops when the handle is dropped.
    pub fn spawn(self) -> IngestionHandle<Cryptarchia::Tx> {
        let ((local_blocks, proposals, polled_tips, era_changes), inputs) = channels();
        let task = spawn(
            "logos/chain/block-ingestion",
            self.run(inputs)
                .instrument(span!(target: LOG_TARGET, Level::TRACE, SERVICE_ID)),
        );
        IngestionHandle {
            local_blocks,
            proposals,
            polled_tips,
            era_changes,
            task: task.into(),
        }
    }

    /// Takes the node's own blocks first: they've won a slot, which is lost if
    /// they aren't published soon.
    async fn run(mut self, mut inputs: Inputs<Cryptarchia::Tx>) {
        loop {
            tokio::select! {
                biased;

                Some(LocalBlock { block, resp }) = inputs.local_blocks.recv() => {
                    let result =
                        apply_block_and_reconcile_mempool(block, &self.cryptarchia, &self.mempool)
                            .await;
                    if let Err(send_err) = resp.send(result) {
                        error!(
                            target: LOG_TARGET,
                            "Failed to send ApplyBlockAndReconcileMempool response: {:?}", send_err
                        );
                    }
                }

                Some(change) = inputs.era_changes.recv() => self.follow_era(change).await,

                Some(proposal) = inputs.proposals.recv() => {
                    self.handle_incoming_proposal(proposal).await;
                }

                Some(polled) = inputs.polled_tips.recv() => {
                    self.enqueue_polled_tip(polled);
                }

                Some(block) = self.orphan_downloader.next(), if self.orphan_downloader.should_poll() => {
                    self.handle_downloaded_orphan(block).await;
                }
            }
        }
    }

    fn orphan_downloader(&mut self) -> &mut OrphanBlocksDownloader<NetAdapter, RuntimeServiceId> {
        self.orphan_downloader.as_mut().get_mut()
    }

    async fn follow_era(&mut self, change: EraChange) {
        match change {
            EraChange::Started(era) => {
                self.orphan_downloader().rotate_era(era).await;
                self.mempool.rotate_era(era).await;
            }
            EraChange::TransitionEnded(ended_era) => {
                self.orphan_downloader()
                    .complete_era_transition(ended_era)
                    .await;
                self.mempool.complete_era_transition(ended_era).await;
            }
        }
    }

    async fn handle_incoming_proposal(&mut self, proposal: Proposal) {
        let block_id = proposal.header().id();
        let block_slot = proposal.header().slot();
        metrics::consensus_proposals_received_total("network");

        match should_process_block(&self.cryptarchia, block_id, block_slot).await {
            Ok(()) => {}
            Err(DoNotProcessBlock::OlderThanLib) => {
                metrics::consensus_proposals_ignored_total("older_than_lib", "network");
                self.orphan_downloader().insert_rejected_block(block_id);
                return;
            }
            Err(DoNotProcessBlock::AlreadyApplied) => {
                metrics::consensus_proposals_ignored_total("already_processed", "network");
                return;
            }
        }

        // Check the proposal before any mempool scanning.
        if let Err(e) = verify_proposal_and_cache_rejected(&proposal, self.orphan_downloader()) {
            metrics::consensus_observe_proposal_reconstruct_err("network", &e);
            return;
        }

        let reconstruct_started_at = Instant::now();
        let block = match reconstruct_block_from_proposal(
            proposal,
            *self.block_limits.at_slot(block_slot).parameters,
            &self.mempool,
        )
        .await
        {
            Ok(block) => {
                metrics::consensus_observe_proposal_reconstruct_ok(
                    reconstruct_started_at.elapsed(),
                );
                block
            }
            Err(e) => {
                metrics::consensus_observe_proposal_reconstruct_err("network", &e);
                error!(
                    target: LOG_TARGET,
                    "Failed to reconstruct block from proposal: {:?}",
                    e
                );
                // Deliberately not recorded against `block_id`. Failing to
                // resolve a reference says the transaction has not reached
                // *this* mempool, not that the block is invalid; the same block
                // may arrive in full through chain synchronisation and must
                // then be judged on its merits.
                return;
            }
        };

        log_received_block(&block);
        let source = BlockSource::Proposal {
            started_at: Instant::now(),
        };
        self.apply_block(block, source).await;
    }

    async fn handle_downloaded_orphan(&mut self, block: Block<Cryptarchia::Tx>) {
        let header_id = block.header().id();
        debug!(
            target: LOG_TARGET,
            "Processing block from orphan downloader: {header_id:?}"
        );

        match should_process_block(&self.cryptarchia, header_id, block.header().slot()).await {
            Ok(()) => {}
            Err(DoNotProcessBlock::OlderThanLib) => {
                self.orphan_downloader().insert_rejected_block(header_id);
                return;
            }
            Err(DoNotProcessBlock::AlreadyApplied) => return,
        }

        log_received_block(&block);
        self.apply_block(block, BlockSource::OrphanDownload).await;
    }

    /// Applies a block and handles the outcome.
    async fn apply_block(&mut self, block: Block<Cryptarchia::Tx>, source: BlockSource) {
        let block_id = block.header().id();
        let result =
            apply_block_and_reconcile_mempool(block, &self.cryptarchia, &self.mempool).await;
        self.finish_block(result, block_id, source);
    }

    fn finish_block(&mut self, result: Result<(), Error>, block_id: HeaderId, source: BlockSource) {
        match (source, result) {
            (BlockSource::Proposal { started_at }, Ok(())) => {
                metrics::consensus_observe_apply_block_ok(started_at.elapsed());
                self.orphan_downloader().remove_orphan(&block_id);
                trace!(target: LOG_TARGET, {
                    counter.consensus_processed_blocks = 1
                }, "Processed consensus block");
            }
            (BlockSource::Proposal { .. }, Err(err)) => {
                metrics::consensus_observe_apply_block_err(&err);
                self.handle_proposal_processing_error(err, block_id);
            }
            (BlockSource::OrphanDownload, Ok(())) => {
                trace!(target: LOG_TARGET, {
                    counter.consensus_processed_blocks = 1
                }, "Processed consensus block");
            }
            (BlockSource::OrphanDownload, Err(e)) => {
                error!(target: LOG_TARGET, "Error processing orphan downloader block: {e:?}");
                if !is_recoverable_apply_error(&e) {
                    self.orphan_downloader().insert_rejected_block(block_id);
                }
                self.orphan_downloader().cancel_active_download();
            }
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "Keep proposal error handling in one match."
    )]
    fn handle_proposal_processing_error(&mut self, err: Error, block_id: HeaderId) {
        match err {
            Error::Cryptarchia(ApiError::ParentMissing { parent, info }) => {
                debug!(
                    target: LOG_TARGET, ?block_id, ?parent,
                    "Parent block missing. Trying to enqueue block for orphan processing",
                );
                if let Err(e) = self.orphan_downloader().enqueue_orphan(
                    block_id,
                    Some(parent),
                    info.tip,
                    info.lib,
                ) {
                    error!(
                        target: LOG_TARGET, %e, ?block_id, ?parent,
                        "Failed to enqueue block for orphan processing",
                    );
                }
            }
            Error::Cryptarchia(ApiError::FutureBlock {
                block_slot,
                current_slot,
            }) => {
                debug!(
                    target: LOG_TARGET, ?block_id, ?block_slot, ?current_slot,
                    "Dropping block from a future slot; it is fetched again once a descendant arrives",
                );
            }
            err => {
                error!(
                    target: LOG_TARGET, %err, ?block_id,
                    "Error processing reconstructed block",
                );
                if !is_recoverable_apply_error(&err) {
                    self.orphan_downloader().insert_rejected_block(block_id);
                }
            }
        }
    }

    /// Hand a tip discovered by the watchdog to the orphan downloader and
    /// record the outcome.
    fn enqueue_polled_tip(&mut self, polled: PolledTip) {
        let PolledTip { tip, height, local } = polled;
        match self
            .orphan_downloader()
            .enqueue_orphan(tip, None, local.tip, local.lib)
        {
            Ok(()) => {
                info!(
                    target: LOG_TARGET,
                    ?tip, height, local_height = local.height,
                    "tip poll: enqueued peer tip for catch-up"
                );
                metrics::tip_poll_enqueued_total();
            }
            Err(e) => {
                debug!(target: LOG_TARGET, %e, ?tip, "tip poll: did not enqueue polled tip");
            }
        }
    }
}

fn log_received_block<Tx>(block: &Block<Tx>)
where
    Tx: SignedMantleTx<Preverified, StandardMode> + StorageSize,
{
    let content_size = 0; // TODO: calculate the actual content size
    let transactions = block.transactions_iter().len();

    trace!(target: LOG_TARGET, {
        counter.received_blocks = 1,
        transactions = transactions,
        bytes = content_size
    }, "Received block");
    trace!(target: LOG_TARGET, {
        histogram.received_blocks_data = content_size,
        transactions = transactions,
    }, "Recorded received block size");
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZero, NonZeroUsize},
        time::Duration,
    };

    use lb_chain_service::{AppliedBlock, ConsensusMsg, Error as ChainError};
    use lb_core::mantle::{SignedOps, TxHash};
    use lb_time::era::{EraEntriesAfterGenesis, EraTiming};
    use lb_tx_service::MempoolMsg;
    use overwatch::services::{
        ServiceData,
        relay::{InboundRelay, Relay},
        state::{NoOperator, NoState},
    };
    use tokio::time::timeout;

    use super::*;
    use crate::tests::{NoopNetworkAdapter, block};

    const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

    type Tx = SignedOps<Preverified, StandardMode>;
    type ApplyReply = oneshot::Sender<Result<AppliedBlock<Tx>, ChainError>>;

    struct Chain;

    impl ServiceData for Chain {
        type Settings = ();
        type State = NoState<()>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ConsensusMsg<Tx>;
    }

    /// The chain service's end of the relay.
    struct StubChain(InboundRelay<ConsensusMsg<Tx>>);

    impl StubChain {
        /// The next block to apply. Queries go unanswered, which ingestion
        /// takes as nothing being against applying the block.
        async fn next_block(&mut self) -> (HeaderId, ApplyReply) {
            loop {
                let request = timeout(REPLY_TIMEOUT, self.0.recv()).await.unwrap();
                if let Some(ConsensusMsg::ApplyBlock {
                    block,
                    reply_channel,
                }) = request
                {
                    return (block.header().id(), reply_channel);
                }
            }
        }
    }

    struct Started {
        chain: StubChain,
        ingestion: BlockIngestion<Chain, NoopNetworkAdapter<Block<Tx>>, usize>,
        _mempool: InboundRelay<MempoolMsg<HeaderId, Tx, Tx, TxHash>>,
    }

    fn started() -> Started {
        let Relay {
            inbound_relay: chain,
            outbound_relay: chain_relay,
            ..
        } = Relay::new(16);
        let Relay {
            inbound_relay: mempool,
            outbound_relay: mempool_relay,
            ..
        } = Relay::new(16);
        let ingestion = BlockIngestion::new(
            CryptarchiaServiceApi::new(chain_relay),
            MempoolAdapter::new(mempool_relay),
            OrphanBlocksDownloader::new(NoopNetworkAdapter::new(), NonZeroUsize::MIN, 1),
            block_limits(),
        );
        Started {
            chain: StubChain(chain),
            ingestion,
            _mempool: mempool,
        }
    }

    /// A single era, whose blocks may hold anything.
    fn block_limits() -> EraSchedule<BlockLimits> {
        let timing = EraTiming {
            slot_duration: Duration::from_secs(1),
            epoch_length: NonZero::new(100).unwrap(),
        };
        EraSchedule::new(
            time::OffsetDateTime::UNIX_EPOCH,
            timing,
            EraEntriesAfterGenesis::empty(),
        )
        .unwrap()
        .map(|_| BlockLimits {
            max_uncles: u8::MAX,
            max_transactions: u16::MAX,
            max_transactions_size: u32::MAX,
        })
    }

    #[tokio::test]
    async fn a_local_block_goes_ahead_of_queued_proposals() {
        let Started {
            mut chain,
            ingestion,
            _mempool,
        } = started();
        let ((local_blocks, proposals, _polled_tips, _era_changes), inputs) = channels();
        let proposal = block::<Tx>(1);
        let local = block(2);
        let local_id = local.header().id();

        proposals.try_send(proposal.to_proposal()).unwrap();
        let (resp, _result) = oneshot::channel();
        assert!(
            local_blocks
                .try_send(LocalBlock { block: local, resp })
                .is_ok()
        );
        let _task: CancellableHandle<()> = spawn("test/ingestion", ingestion.run(inputs)).into();

        assert_eq!(chain.next_block().await.0, local_id);
    }
}
