//! Applies blocks one at a time, in a future the service polls along with its
//! other inputs, so that however long a block takes, the service keeps taking
//! in what peers and other services send it.

use core::{convert::Infallible, fmt::Debug, future, pin::Pin};
use std::time::Instant;

use futures::{
    FutureExt as _, StreamExt as _,
    future::{BoxFuture, OptionFuture},
};
use lb_chain_service::{
    BlockWithVerifiedHeader,
    api::{ApiError, CryptarchiaServiceApi, CryptarchiaServiceData},
};
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
use tokio::sync::{mpsc, oneshot};
use tracing::{Level, debug, error, info, trace, warn};

use crate::{
    DoNotProcessBlock, Error, LOG_TARGET, apply_block_and_reconcile_mempool, apply_block_to_chain,
    apply_block_with_verified_header_and_reconcile_mempool, is_recoverable_apply_error,
    mempool::{MempoolAdapter as _, adapter::MempoolAdapter},
    metrics,
    network::NetworkAdapter,
    reconcile_mempool, reconstruct_block_from_proposal, should_process_block,
    sync::{orphan_handler::OrphanBlocksDownloader, tip_poll::PolledTip},
};

/// How many received proposals wait for ingestion. They're dropped past this,
/// so that a flood of them can't hold anything else up. Catching up on a
/// dropped one is the same as on one that gossip never delivered.
const MAX_QUEUED_PROPOSALS: usize = 64;

/// The chain leader waits for each of its blocks, so few are ever queued. While
/// this many are, the service leaves its messages on its relay.
const MAX_QUEUED_LOCAL_BLOCKS: usize = 4;

const MAX_QUEUED_POLLED_TIPS: usize = 32;

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

/// What peers send, and what chain recovery takes in: the inputs that take
/// turns.
enum NetworkInput<Tx> {
    Proposal(Proposal),
    PolledTip(PolledTip),
    Orphan(Block<Tx>),
}

/// A block from the network being checked, and, if it came as a proposal, its
/// header verified and the block rebuilt from the mempool. That changes
/// nothing, so it can run while other blocks are applied.
type Preparation<Tx> = BoxFuture<'static, Prepared<Tx>>;

/// What a [`Preparation`] comes to.
enum Prepared<Tx> {
    /// A proposal's block, its header verified and the block rebuilt, ready to
    /// apply.
    Proposal(BlockWithVerifiedHeader<Tx>),
    /// A downloaded block ready to apply.
    Orphan(Block<Tx>),
    /// A proposal whose header failed verification, which the orphan
    /// downloader hears of as of a block that failed to apply.
    Unverified { block_id: HeaderId, error: Error },
    /// A block at or before the LIB, which the orphan downloader isn't to
    /// fetch again.
    OlderThanLib(HeaderId),
    /// A block in the chain already, or a proposal that couldn't be rebuilt.
    Skipped,
}

/// Block ingestion, as seen by the service, which hands it inputs without ever
/// waiting on it: ingestion makes progress only while the service polls it.
pub struct IngestionHandle<Tx> {
    local_blocks: mpsc::Sender<LocalBlock<Tx>>,
    proposals: mpsc::Sender<Proposal>,
    polled_tips: mpsc::Sender<PolledTip>,
    /// Unbounded, as there are two era changes per era.
    era_changes: mpsc::UnboundedSender<EraChange>,
}

impl<Tx> IngestionHandle<Tx> {
    /// Queues `proposal` for ingestion, unless too many are queued already.
    pub fn enqueue_proposal(&self, proposal: Proposal) {
        if let Err(error) = self.proposals.try_send(proposal) {
            warn!(target: LOG_TARGET, %error, "Dropping a block proposal");
            metrics::consensus_proposals_ignored_total("ingestion_queue_full", "network");
        }
    }

    /// Whether a local block can be queued now.
    pub fn has_room_for_local_block(&self) -> bool {
        self.local_blocks.capacity() > 0
    }

    /// Queues the node's own `block`, which
    /// [`has_room_for_local_block`](Self::has_room_for_local_block) has made
    /// sure there's room for.
    pub fn apply_local_block(&self, block: Block<Tx>, resp: oneshot::Sender<Result<(), Error>>) {
        if let Err(error) = self.local_blocks.try_send(LocalBlock { block, resp }) {
            error!(target: LOG_TARGET, %error, "Dropping a local block");
        }
    }

    pub fn polled_tips(&self) -> mpsc::Sender<PolledTip> {
        self.polled_tips.clone()
    }

    pub fn rotate_era(&self, era: Era) {
        self.report_era_change(EraChange::Started(era));
    }

    pub fn complete_era_transition(&self, ended_era: Era) {
        self.report_era_change(EraChange::TransitionEnded(ended_era));
    }

    fn report_era_change(&self, change: EraChange) {
        if self.era_changes.send(change).is_err() {
            error!(target: LOG_TARGET, "Block ingestion is gone: dropping an era change");
        }
    }
}

struct Inputs<Tx> {
    local_blocks: mpsc::Receiver<LocalBlock<Tx>>,
    proposals: mpsc::Receiver<Proposal>,
    polled_tips: mpsc::Receiver<PolledTip>,
    era_changes: mpsc::UnboundedReceiver<EraChange>,
}

type Senders<Tx> = (
    mpsc::Sender<LocalBlock<Tx>>,
    mpsc::Sender<Proposal>,
    mpsc::Sender<PolledTip>,
    mpsc::UnboundedSender<EraChange>,
);

fn channels<Tx>() -> (Senders<Tx>, Inputs<Tx>) {
    let (local_blocks_tx, local_blocks) = mpsc::channel(MAX_QUEUED_LOCAL_BLOCKS);
    let (proposals_tx, proposals) = mpsc::channel(MAX_QUEUED_PROPOSALS);
    let (polled_tips_tx, polled_tips) = mpsc::channel(MAX_QUEUED_POLLED_TIPS);
    let (era_changes_tx, era_changes) = mpsc::unbounded_channel();
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

    /// The handle the service hands inputs over with, and ingestion itself,
    /// which the service polls along with its other inputs and which never
    /// ends.
    pub fn start(
        self,
    ) -> (
        IngestionHandle<Cryptarchia::Tx>,
        impl Future<Output = Infallible> + Send,
    ) {
        let ((local_blocks, proposals, polled_tips, era_changes), inputs) = channels();
        let handle = IngestionHandle {
            local_blocks,
            proposals,
            polled_tips,
            era_changes,
        };
        (handle, self.run(inputs))
    }

    /// Takes the node's own blocks first: they've won a slot, which is lost if
    /// they aren't published soon. Era changes come next. What peers send and
    /// chain recovery take turns after them, so that neither can hold the other
    /// up for good.
    ///
    /// A proposal and a downloaded block are checked, and the proposal
    /// verified and rebuilt, side by side and while everything else keeps
    /// going, so a slow rebuild holds up neither the node's own blocks nor
    /// chain recovery. Only applying a block takes its turn. The next proposal
    /// waits for the one being prepared, and the next downloaded block for its
    /// own, so blocks are still applied one at a time, and those from one
    /// source in the order they're taken.
    async fn run(mut self, mut inputs: Inputs<Cryptarchia::Tx>) -> Infallible {
        let mut proposal: Option<Preparation<Cryptarchia::Tx>> = None;
        let mut orphan: Option<Preparation<Cryptarchia::Tx>> = None;
        loop {
            let (take_proposal, take_orphan) = (proposal.is_none(), orphan.is_none());
            tokio::select! {
                biased;

                Some(LocalBlock { block, resp }) = inputs.local_blocks.recv() => {
                    Self::apply_local_block(block, resp, &self.cryptarchia, &self.mempool).await;
                }

                Some(change) = inputs.era_changes.recv() => self.follow_era(change).await,

                Some(prepared) = OptionFuture::from(proposal.as_mut()) => {
                    proposal = None;
                    self.apply_prepared(prepared).await;
                }

                Some(prepared) = OptionFuture::from(orphan.as_mut()) => {
                    orphan = None;
                    self.apply_prepared(prepared).await;
                }

                input = Self::next_network_input(
                    &mut inputs.proposals,
                    &mut inputs.polled_tips,
                    &mut self.orphan_downloader,
                    take_proposal,
                    take_orphan,
                ) => match input {
                    NetworkInput::Proposal(received) => {
                        proposal = Some(self.prepare_proposal(received));
                    }
                    NetworkInput::PolledTip(polled) => self.enqueue_polled_tip(polled),
                    NetworkInput::Orphan(block) => orphan = Some(self.prepare_orphan(block)),
                },
            }
        }
    }

    /// The next proposal, polled tip or downloaded orphan, picked at random
    /// among those ready. Proposals are taken only if `take_proposal`, and
    /// downloaded blocks only if `take_orphan`.
    async fn next_network_input(
        proposals: &mut mpsc::Receiver<Proposal>,
        polled_tips: &mut mpsc::Receiver<PolledTip>,
        orphan_downloader: &mut Pin<Box<OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>>>,
        take_proposal: bool,
        take_orphan: bool,
    ) -> NetworkInput<Cryptarchia::Tx> {
        tokio::select! {
            Some(proposal) = proposals.recv(), if take_proposal => NetworkInput::Proposal(proposal),
            Some(polled) = polled_tips.recv() => NetworkInput::PolledTip(polled),
            Some(block) = orphan_downloader.next(), if take_orphan && orphan_downloader.should_poll() => {
                NetworkInput::Orphan(block)
            }
            // Their senders are gone only as the service stops.
            else => future::pending().await,
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

    /// Starts checking `proposal`, verifying its header and rebuilding its
    /// block.
    fn prepare_proposal(&self, proposal: Proposal) -> Preparation<Cryptarchia::Tx> {
        metrics::consensus_proposals_received_total("network");

        let block_limits = *self
            .block_limits
            .at_slot(proposal.header().slot())
            .parameters;
        let cryptarchia = self.cryptarchia.clone();
        let mempool = self.mempool.clone();
        async move {
            let block_id = proposal.header().id();
            match should_process_block(&cryptarchia, block_id, proposal.header().slot()).await {
                Ok(()) => {}
                Err(DoNotProcessBlock::OlderThanLib) => {
                    metrics::consensus_proposals_ignored_total("older_than_lib", "network");
                    return Prepared::OlderThanLib(block_id);
                }
                Err(DoNotProcessBlock::AlreadyApplied) => {
                    metrics::consensus_proposals_ignored_total("already_processed", "network");
                    return Prepared::Skipped;
                }
            }

            // Verify the whole header before any mempool scanning.
            let header = match cryptarchia
                .verify_header(proposal.header().clone(), proposal.uncle_headers().clone())
                .await
                .map_err(Error::from)
            {
                Ok(header) => header,
                // Applied since it was checked, as a downloaded block.
                Err(Error::Cryptarchia(ApiError::AlreadyApplied(_))) => {
                    metrics::consensus_proposals_ignored_total("already_processed", "network");
                    return Prepared::Skipped;
                }
                Err(error) => {
                    metrics::consensus_observe_proposal_reconstruct_err("network", &error);
                    return Prepared::Unverified { block_id, error };
                }
            };

            let reconstruct_started_at = Instant::now();
            match reconstruct_block_from_proposal(
                header,
                proposal.mempool_transactions(),
                block_limits,
                &mempool,
            )
            .await
            {
                Ok(block) => {
                    metrics::consensus_observe_proposal_reconstruct_ok(
                        reconstruct_started_at.elapsed(),
                    );
                    Prepared::Proposal(block)
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
                    // *this* mempool, not that the block is invalid; the same
                    // block may arrive in full through chain synchronisation
                    // and must then be judged on its merits.
                    Prepared::Skipped
                }
            }
        }
        .boxed()
    }

    /// Starts checking whether a downloaded block is still to be applied.
    fn prepare_orphan(&self, block: Block<Cryptarchia::Tx>) -> Preparation<Cryptarchia::Tx> {
        let cryptarchia = self.cryptarchia.clone();
        async move {
            let header_id = block.header().id();
            debug!(
                target: LOG_TARGET,
                "Processing block from orphan downloader: {header_id:?}"
            );

            match should_process_block(&cryptarchia, header_id, block.header().slot()).await {
                Ok(()) => Prepared::Orphan(block),
                Err(DoNotProcessBlock::OlderThanLib) => Prepared::OlderThanLib(header_id),
                Err(DoNotProcessBlock::AlreadyApplied) => Prepared::Skipped,
            }
        }
        .boxed()
    }

    async fn apply_prepared(&mut self, prepared: Prepared<Cryptarchia::Tx>) {
        match prepared {
            Prepared::Proposal(block) => {
                log_received_block(block.transactions_iter());
                let block_id = block.header().id();
                let started_at = Instant::now();
                let result = apply_block_with_verified_header_and_reconcile_mempool(
                    block,
                    &self.cryptarchia,
                    &self.mempool,
                )
                .await;
                self.finish_applying(result, block_id, BlockSource::Proposal { started_at });
            }
            Prepared::Orphan(block) => {
                log_received_block(block.transactions_iter());
                self.apply_block(block, BlockSource::OrphanDownload).await;
            }
            Prepared::Unverified { block_id, error } => {
                self.handle_proposal_processing_error(error, block_id);
            }
            Prepared::OlderThanLib(block_id) => {
                self.orphan_downloader().insert_rejected_block(block_id);
            }
            Prepared::Skipped => {}
        }
    }

    /// Applies the node's own `block` and replies as soon as it's in the chain,
    /// as it is to be published soon. The mempool follows it after the reply.
    async fn apply_local_block(
        block: Block<Cryptarchia::Tx>,
        resp: oneshot::Sender<Result<(), Error>>,
        cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
        mempool: &MempoolAdapter<Cryptarchia::Tx>,
    ) {
        let (result, applied) = match apply_block_to_chain(block, cryptarchia).await {
            Ok(applied) => (Ok(()), Some(applied)),
            Err(error) => (Err(error), None),
        };
        if let Err(send_err) = resp.send(result) {
            error!(
                target: LOG_TARGET,
                "Failed to send ApplyBlockAndReconcileMempool response: {:?}", send_err
            );
        }
        if let Some((applied, block_id, block_slot)) = applied {
            reconcile_mempool(applied, block_id, block_slot, mempool).await;
        }
    }

    /// Applies a block and handles the outcome.
    async fn apply_block(&mut self, block: Block<Cryptarchia::Tx>, source: BlockSource) {
        let block_id = block.header().id();
        let result =
            apply_block_and_reconcile_mempool(block, &self.cryptarchia, &self.mempool).await;
        self.finish_applying(result, block_id, source);
    }

    fn finish_applying(
        &mut self,
        result: Result<(), Error>,
        block_id: HeaderId,
        source: BlockSource,
    ) {
        match result {
            // A proposal and a downloaded block are prepared side by side, so
            // the same block can come both ways, and the second to be applied
            // finds it in the chain. That says nothing against the rest of a
            // download.
            Err(Error::Cryptarchia(ApiError::AlreadyApplied(_))) => {
                debug!(target: LOG_TARGET, ?block_id, "Block applied already");
            }
            result => self.finish_block(result, block_id, source),
        }
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
                    "Error processing block from proposal",
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

/// Records a received block's transaction count and content size: the encoded
/// size of its transactions, which takes a walk over them, so only when the
/// records are taken.
fn log_received_block<'tx, Tx, Transactions>(transactions: Transactions)
where
    Tx: StorageSize + 'tx,
    Transactions: ExactSizeIterator<Item = &'tx Tx>,
{
    if !tracing::enabled!(target: LOG_TARGET, Level::TRACE) {
        return;
    }
    let count = transactions.len();
    let content_size: usize = transactions.map(StorageSize::storage_size).sum();

    trace!(target: LOG_TARGET, {
        counter.received_blocks = 1,
        transactions = count,
        bytes = content_size
    }, "Received block");
    trace!(target: LOG_TARGET, {
        histogram.received_blocks_data = content_size,
        transactions = count,
    }, "Recorded received block size");
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZero, NonZeroUsize},
        time::Duration,
    };

    use lb_chain_service::{
        ConsensusMsg, CryptarchiaInfo, Error as ChainError, Query, VerifiedBlockHeader,
    };
    use lb_core::mantle::{SignedOps, TxHash};
    use lb_cryptarchia_engine::{Slot, State};
    use lb_time::era::{EraEntriesAfterGenesis, EraTiming};
    use lb_tx_service::MempoolMsg;
    use lb_utils::tokio::task::{CancellableHandle, spawn};
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
    type Ingestion = BlockIngestion<Chain, NoopNetworkAdapter<Block<Tx>>, usize>;
    type HeaderCheckReply = oneshot::Sender<Result<VerifiedBlockHeader, ChainError>>;

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
        /// The next block whose header is checked, which is the first step of
        /// applying a block, and where the check's answer goes. Other requests
        /// go unanswered, which ingestion takes as nothing being against the
        /// block until its header is checked.
        async fn next_header_check(&mut self) -> (HeaderId, HeaderCheckReply) {
            loop {
                let request = timeout(REPLY_TIMEOUT, self.0.recv()).await.unwrap();
                if let Some(ConsensusMsg::Query(Query::VerifyHeader {
                    header,
                    reply_channel,
                    ..
                })) = request
                {
                    return (header.id(), reply_channel);
                }
            }
        }
    }

    struct Started {
        chain: StubChain,
        ingestion: Ingestion,
        /// The mempool's end of the relay.
        mempool: InboundRelay<MempoolMsg<HeaderId, Tx, Tx, TxHash>>,
    }

    /// Ingestion, whose orphan downloader gets `download` for whatever it
    /// asks.
    fn started(download: Vec<(HeaderId, Block<Tx>)>) -> Started {
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
            OrphanBlocksDownloader::new(NoopNetworkAdapter::new(download), NonZeroUsize::MIN, 1),
            block_limits(),
        );
        Started {
            chain: StubChain(chain),
            ingestion,
            mempool,
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
            mempool: _mempool,
        } = started(Vec::new());
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
        let _task: CancellableHandle<Infallible> =
            spawn("test/ingestion", ingestion.run(inputs)).into();

        assert_eq!(chain.next_header_check().await.0, local_id);
    }

    /// The node's own block doesn't wait for a proposal whose header the chain
    /// service is still verifying.
    #[tokio::test]
    async fn a_local_block_goes_ahead_of_a_proposal_being_verified() {
        let Started {
            mut chain,
            ingestion,
            mempool: _mempool,
        } = started(Vec::new());
        let ((local_blocks, proposals, _polled_tips, _era_changes), inputs) = channels();
        let proposal = block::<Tx>(1);
        let proposal_id = proposal.header().id();
        let local = block(2);
        let local_id = local.header().id();
        let _task: CancellableHandle<Infallible> =
            spawn("test/ingestion", ingestion.run(inputs)).into();

        proposals.try_send(proposal.to_proposal()).unwrap();
        let (checked, _verification) = chain.next_header_check().await;
        assert_eq!(checked, proposal_id);

        let (resp, _result) = oneshot::channel();
        assert!(
            local_blocks
                .try_send(LocalBlock { block: local, resp })
                .is_ok()
        );
        assert_eq!(chain.next_header_check().await.0, local_id);
    }

    /// Chain recovery goes on while the chain service is still verifying a
    /// proposal's header.
    #[tokio::test]
    async fn recovery_goes_on_while_a_proposal_is_being_verified() {
        let orphan = block::<Tx>(2);
        let orphan_id = orphan.header().id();
        let Started {
            mut chain,
            ingestion,
            mempool: _mempool,
        } = started(vec![(orphan_id, orphan)]);
        let ((_local_blocks, proposals, polled_tips, _era_changes), inputs) = channels();
        let proposal = block::<Tx>(1);
        let proposal_id = proposal.header().id();
        let _task: CancellableHandle<Infallible> =
            spawn("test/ingestion", ingestion.run(inputs)).into();

        proposals.try_send(proposal.to_proposal()).unwrap();
        let (checked, _verification) = chain.next_header_check().await;
        assert_eq!(checked, proposal_id);

        polled_tips.try_send(polled_tip(orphan_id)).unwrap();
        assert_eq!(chain.next_header_check().await.0, orphan_id);
    }

    /// A peer's tip at `tip`, as the tip poller hands it over.
    fn polled_tip(tip: HeaderId) -> PolledTip {
        let genesis = HeaderId::from([0; 32]);
        PolledTip {
            tip,
            height: 1,
            local: CryptarchiaInfo {
                lib: genesis,
                lib_slot: Slot::genesis(),
                tip: genesis,
                slot: Slot::genesis(),
                height: 0,
                state: State::Online,
            },
        }
    }

    /// Proposals that never let up still leave chain recovery its turns.
    #[tokio::test]
    async fn proposals_that_keep_coming_leave_recovery_its_turn() {
        let ((_, proposals, polled_tips, _), mut inputs) = channels::<Tx>();
        let mut orphan_downloader = Box::pin(OrphanBlocksDownloader::new(
            NoopNetworkAdapter::new(Vec::new()),
            NonZeroUsize::MIN,
            1,
        ));
        let proposal = block::<Tx>(1).to_proposal();
        while proposals.try_send(proposal.clone()).is_ok() {}
        polled_tips
            .try_send(polled_tip(HeaderId::from([1; 32])))
            .unwrap();

        let tip_taken = async {
            loop {
                match Ingestion::next_network_input(
                    &mut inputs.proposals,
                    &mut inputs.polled_tips,
                    &mut orphan_downloader,
                    true,
                    true,
                )
                .await
                {
                    NetworkInput::PolledTip(_) => break,
                    NetworkInput::Proposal(_) => proposals.try_send(proposal.clone()).unwrap(),
                    NetworkInput::Orphan(_) => {}
                }
            }
        };

        timeout(REPLY_TIMEOUT, tip_taken)
            .await
            .expect("The polled tip should get its turn.");
    }
}
