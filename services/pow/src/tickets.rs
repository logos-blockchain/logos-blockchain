//! Proof-of-Work reward ticket generation.
//!
//! Watches the chain for newly processed blocks and, for every block still
//! within the reward window, runs a concurrent search for a "winning" ticket:
//! a random key whose derived puzzle ticket meets the block's difficulty
//! target. Winning tickets are surfaced through the [`TicketGenerator`] stream.

use std::{
    collections::{HashMap, HashSet},
    iter,
    num::NonZeroUsize,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt as _, stream};
use lb_chain_service::{
    ConsensusMsg, EpochState, ProcessedBlockEvent, Slot, api::CryptarchiaServiceApi,
};
use lb_core::{
    crypto::ZkHash,
    header::HeaderId,
    mantle::ops::pow::{ClaimPowRewardOp, PowTarget},
};
use lb_key_management_system_keys::keys::UnsecuredZkKey;
use lb_ledger::LedgerState;
use lb_log_targets::pow;
use rayon::ThreadPool;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_stream::{
    StreamMap,
    wrappers::{BroadcastStream, errors::BroadcastStreamRecvError},
};
use tracing::{error, log::warn};

const LOG_TARGET: &str = pow::ROOT;

/// A winning Proof-of-Work reward: the secret key that produced the ticket,
/// paired with the reward-claim operation to be published.
pub type WinnerTicket = (UnsecuredZkKey, ClaimPowRewardOp);
/// A boxed stream of [`WinnerTicket`]s found for a single block, each tagged
/// with that block's slot (for reward-window bookkeeping).
pub type WinnerTicketStream = Pin<Box<dyn Stream<Item = (Slot, WinnerTicket)> + Send>>;

/// A winning ticket together with the chain tip observed when it was found.
///
/// The generator caches the tip from its block-event stream, so consumers can
/// size the reward-claim transaction against the current state without an extra
/// round-trip to the chain service.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinningTicket {
    /// Chain tip when the ticket was produced.
    pub tip: HeaderId,
    /// Slot of the block the claim is anchored to, used to check whether the
    /// ticket is still within the reward window.
    pub block_slot: Slot,
    /// Secret key that produced the winning claim (owns the reward note).
    pub secret_key: UnsecuredZkKey,
    /// The reward-claim operation to publish.
    pub claim: ClaimPowRewardOp,
}

/// A [`Stream`] of winning Proof-of-Work reward tickets.
///
/// It subscribes to the chain's processed-block events and drives, per block,
/// an independent concurrent search for winning tickets. Searches for blocks
/// that fall out of the reward window are pruned as the tip advances.
pub struct TicketGenerator {
    /// Stream of processed blocks, each enriched with the epoch and ledger
    /// state required to search for tickets. Owns the only clone of the chain
    /// API handle, used to fetch that state.
    processed_block_stream:
        Pin<Box<dyn Stream<Item = (EpochState, LedgerState, ProcessedBlockEvent)> + Send>>,
    /// Ongoing per-block ticket searches, keyed by block header. Each entry
    /// yields winning tickets for that block as they are found.
    tickets_search: StreamMap<HeaderId, WinnerTicketStream>,
    /// Index from a block's slot to the headers with an active search at that
    /// slot, used to prune searches once their block leaves the reward window.
    tickets_search_by_slot: HashMap<Slot, HashSet<HeaderId>>,
    /// Chain tip from the most recently processed block, cached so emitted
    /// tickets carry the current tip without the consumer having to query it.
    /// Initialized to the genesis id and overwritten by the first processed
    /// block, which always precedes any emitted ticket.
    tip: HeaderId,

    /// Shared pool of worker threads dedicated to the CPU-heavy ticket search,
    /// keeping that work off Tokio's runtime threads. Cloned into every active
    /// per-block search so the searches share the same threads.
    pool: Arc<ThreadPool>,
    /// Maximum number of ticket-search attempts kept in flight concurrently for
    /// each block (the `buffer_unordered` degree of every per-block search).
    max_tickets_per_block: NonZeroUsize,
    /// Acceptance window, in slots: a block older than this leaves the reward
    /// window and its search is pruned. Matches the consensus `slot_window`.
    slot_window: u64,
}

impl TicketGenerator {
    /// Creates a new [`TicketGenerator`].
    ///
    /// Subscribes to the chain service's new-block stream and wires the
    /// enrichment pipeline that attaches each block's epoch and ledger state.
    /// The chain API type only appears here; once the block stream is built its
    /// type is erased, so the generator itself is not generic.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription to the chain service fails.
    pub async fn new<Tx, CryptarchiaServiceData, RuntimeServiceId>(
        cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
        pool: Arc<ThreadPool>,
        max_tickets_per_block: NonZeroUsize,
        slot_window: u64,
    ) -> Result<Self, lb_chain_service::api::ApiError>
    where
        CryptarchiaServiceData:
            Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
        RuntimeServiceId: Send + Sync + 'static,
        Tx: Send + Sync + 'static,
    {
        let stream = BroadcastStream::new(cryptarchia_api.subscribe_new_blocks().await?);
        let processed_block_stream: Pin<
            Box<dyn Stream<Item = (EpochState, LedgerState, ProcessedBlockEvent)> + Send>,
        > = Box::pin(stream.filter_map(move |event| {
            let cryptarchia_api = cryptarchia_api.clone();
            async move { process_block_event(event, cryptarchia_api).await }
        }));
        Ok(Self {
            processed_block_stream,
            tickets_search: StreamMap::new(),
            tickets_search_by_slot: HashMap::new(),
            tip: HeaderId::from([0u8; 32]),
            pool,
            max_tickets_per_block,
            slot_window,
        })
    }
}

/// Enriches a raw processed-block event with its epoch and ledger state.
///
/// Returns `None` (dropping the event) when the broadcast subscription lagged,
/// or when the epoch or ledger state for the block cannot be fetched from the
/// chain service.
async fn process_block_event<Tx, CryptarchiaServiceData, RuntimeServiceId>(
    event: Result<ProcessedBlockEvent, BroadcastStreamRecvError>,
    cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
) -> Option<(EpochState, LedgerState, ProcessedBlockEvent)>
where
    CryptarchiaServiceData:
        Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
    RuntimeServiceId: Send + Sync + 'static,
    Tx: Send + Sync + 'static,
{
    match event {
        Ok(
            event @ ProcessedBlockEvent {
                block_id, tip_slot, ..
            },
        ) => {
            let Ok(Ok(epoch_state)) = cryptarchia_api.get_epoch_state(tip_slot).await else {
                warn!(target: LOG_TARGET, "Epoch state not found for block slot: {tip_slot:?}");
                return None;
            };
            let Ok(Some(ledger_state)) = cryptarchia_api.get_ledger_state(block_id).await else {
                warn!(target: LOG_TARGET, "Ledger state not found for block: {block_id:?}");
                return None;
            };
            Some((epoch_state, ledger_state, event))
        }
        Err(e) => {
            error!(target: LOG_TARGET, "Missed new block event due to: {e}");
            None
        }
    }
}

/// Builds an unbounded stream that searches for winning tickets for a single
/// block.
///
/// Up to `max_tickets_per_block` attempts run concurrently; each draws a fresh
/// random key and checks the resulting ticket against the block's difficulty
/// target. The stream yields every winning `(secret key, claim)` pair it finds
/// and never terminates on its own — it is dropped once the block leaves the
/// reward window (see [`prune_out_of_window_streams`]).
fn new_block_search_stream(
    block_header: HeaderId,
    block_slot: Slot,
    epoch_state: &EpochState,
    ledger_state: &LedgerState,
    pool: Arc<ThreadPool>,
    max_tickets_per_block: NonZeroUsize,
) -> WinnerTicketStream {
    let epoch_nonce = epoch_state.nonce;
    let difficulty = ledger_state.mantle_ledger().pow.reward_difficulty();
    #[expect(
        rustc::closure_returning_async_block,
        reason = "`repeat_with` takes a FnMut not an async closure"
    )]
    let tasks = iter::repeat_with(move || {
        search_winner_ticket(block_header, epoch_nonce, difficulty, Arc::clone(&pool))
    });
    let results = stream::iter(tasks).buffer_unordered(max_tickets_per_block.get());
    let winners = tokio_stream::StreamExt::filter_map(results, |maybe_winner| maybe_winner);
    // Tag every winner with the block's slot so the consumer can track the
    // reward window.
    Box::pin(winners.map(move |ticket| (block_slot, ticket)))
}

/// Runs a single ticket-search attempt for a block.
///
/// Generates a random key, builds the reward claim, and validates its puzzle
/// ticket against `difficulty`. The heavy computation is off-loaded to the
/// dedicated `pool` so it does not stall Tokio's runtime threads. Returns the
/// winning `(key, claim)` when the ticket meets the difficulty target,
/// otherwise `None`.
async fn search_winner_ticket(
    block_header: HeaderId,
    epoch_nonce: ZkHash,
    difficulty: PowTarget,
    pool: Arc<ThreadPool>,
) -> Option<(UnsecuredZkKey, ClaimPowRewardOp)> {
    let (response_sender, response_receiver) = oneshot::channel();
    let pool_task = move || {
        let mut rng = rand::thread_rng();
        let sk = UnsecuredZkKey::from_rng(&mut rng);
        let pk = sk.to_public_key();
        let claim = ClaimPowRewardOp {
            epoch_nonce,
            block_hash: block_header.into(),
            public_key: pk,
        };
        let ticket = claim.get_puzzle_ticket();
        let result = ticket
            .validate_difficulty_reward(&difficulty)
            .is_ok()
            .then_some((sk, claim));
        if response_sender.send(result).is_err() {
            error!(target: LOG_TARGET, "Failed to send ticket result: receiver dropped");
        }
    };
    // Ticket computation is heavy, we have a custom separated threadpool for this
    // tasks
    pool.spawn(pool_task);
    // await response on the channel
    response_receiver.await.ok().flatten()
}

/// Drops the ticket searches for every block whose slot is older than
/// `frontier_slot`, i.e. blocks that have fallen out of the reward window and
/// can no longer produce claimable tickets.
fn prune_out_of_window_streams(
    tickets_search: &mut StreamMap<HeaderId, WinnerTicketStream>,
    tickets_search_by_slot: &mut HashMap<Slot, HashSet<HeaderId>>,
    frontier_slot: Slot,
) {
    let to_remove = tickets_search_by_slot
        .extract_if(|k, _| k < &frontier_slot)
        .flat_map(|(_, headers)| headers);
    for header in to_remove {
        tickets_search.remove(&header);
    }
}

impl Stream for TicketGenerator {
    type Item = WinningTicket;

    /// Advances the generator.
    ///
    /// First drains any winning ticket that a per-block search has already
    /// produced, tagging it with the cached chain tip. Otherwise it ingests the
    /// next processed block: it caches the tip, and if the block is still
    /// within the reward window starts a new search for it, always pruning
    /// the searches for blocks that have aged out of the window.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            // 1. Emit any winner an active search has already produced, tagged with the
            //    cached tip.
            if let Poll::Ready(Some((_, (block_slot, (secret_key, claim))))) =
                this.tickets_search.poll_next_unpin(cx)
            {
                return Poll::Ready(Some(WinningTicket {
                    tip: this.tip,
                    block_slot,
                    secret_key,
                    claim,
                }));
            }

            // 2. Otherwise ingest the next processed block.
            match this.processed_block_stream.poll_next_unpin(cx) {
                Poll::Ready(Some((
                    epoch_state,
                    ledger_state,
                    ProcessedBlockEvent {
                        block_id,
                        block_slot,
                        tip,
                        tip_slot,
                        ..
                    },
                ))) => {
                    this.tip = tip;
                    // compute which slot is old enough
                    let frontier_slot = tip_slot.saturating_sub(Slot::new(this.slot_window));
                    // trigger new stream if its new enough
                    if frontier_slot < block_slot {
                        let stream = new_block_search_stream(
                            block_id,
                            block_slot,
                            &epoch_state,
                            &ledger_state,
                            Arc::clone(&this.pool),
                            this.max_tickets_per_block,
                        );
                        this.tickets_search.insert(block_id, stream);
                        this.tickets_search_by_slot
                            .entry(block_slot)
                            .or_default()
                            .insert(block_id);
                    }
                    // prune old enough winning tickets
                    prune_out_of_window_streams(
                        &mut this.tickets_search,
                        &mut this.tickets_search_by_slot,
                        frontier_slot,
                    );
                    // Fall through to the next loop iteration so the freshly
                    // inserted search is polled now: this starts its work and
                    // registers its waker, without depending on an external
                    // re-poll.
                }
                // Upstream is closed: no block will ever arrive again, so stop
                // mining and end the generator, dropping any in-flight searches.
                // Winners already produced were emitted by the poll above.
                Poll::Ready(None) => return Poll::Ready(None),
                // No new block right now: idle until the next one arrives. The
                // polls above registered the wakers that re-poll us on progress
                // (a new block, or a winner from an active search).
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        num::NonZeroUsize,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };

    use futures::{Stream, stream, task::noop_waker_ref};
    use lb_chain_service::{EpochState, ProcessedBlockEvent, Slot};
    use lb_core::{
        header::HeaderId,
        mantle::ops::pow::{ClaimPowRewardOp, SLOT_WINDOW},
    };
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{UnsecuredZkKey, ZkPublicKey};
    use lb_ledger::LedgerState;
    use rayon::{ThreadPool, ThreadPoolBuilder};
    use tokio_stream::StreamMap;

    use super::{
        TicketGenerator, WinnerTicketStream, WinningTicket, prune_out_of_window_streams,
        search_winner_ticket,
    };

    /// A never-resolving search stream, used to populate the map under test.
    fn pending_stream() -> WinnerTicketStream {
        Box::pin(stream::pending())
    }

    /// A small dedicated thread pool for exercising the ticket search.
    fn test_pool() -> Arc<ThreadPool> {
        Arc::new(
            ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .expect("test thread pool should build"),
        )
    }

    /// The genesis/zero field element, reused as a stand-in epoch nonce.
    fn zero_fr() -> Fr {
        *ZkPublicKey::zero().as_fr()
    }

    #[test]
    fn prune_removes_only_searches_below_the_frontier() {
        let old_block = HeaderId::from([1u8; 32]);
        let recent_block = HeaderId::from([2u8; 32]);

        let mut tickets_search: StreamMap<HeaderId, WinnerTicketStream> = StreamMap::new();
        tickets_search.insert(old_block, pending_stream());
        tickets_search.insert(recent_block, pending_stream());

        let mut tickets_search_by_slot: HashMap<Slot, HashSet<HeaderId>> = HashMap::new();
        tickets_search_by_slot
            .entry(Slot::new(5))
            .or_default()
            .insert(old_block);
        tickets_search_by_slot
            .entry(Slot::new(10))
            .or_default()
            .insert(recent_block);

        prune_out_of_window_streams(
            &mut tickets_search,
            &mut tickets_search_by_slot,
            Slot::new(8),
        );

        // The slot-5 search aged out; the slot-10 one is still within the window.
        assert_eq!(tickets_search.len(), 1);
        assert!(!tickets_search.contains_key(&old_block));
        assert!(tickets_search.contains_key(&recent_block));
        assert!(!tickets_search_by_slot.contains_key(&Slot::new(5)));
        assert!(tickets_search_by_slot.contains_key(&Slot::new(10)));
    }

    #[test]
    fn prune_keeps_everything_when_nothing_aged_out() {
        let block = HeaderId::from([3u8; 32]);
        let mut tickets_search: StreamMap<HeaderId, WinnerTicketStream> = StreamMap::new();
        tickets_search.insert(block, pending_stream());
        let mut tickets_search_by_slot: HashMap<Slot, HashSet<HeaderId>> = HashMap::new();
        tickets_search_by_slot
            .entry(Slot::new(10))
            .or_default()
            .insert(block);

        prune_out_of_window_streams(
            &mut tickets_search,
            &mut tickets_search_by_slot,
            Slot::new(10), // frontier == slot, so it is not "below"
        );

        assert!(tickets_search.contains_key(&block));
        assert!(tickets_search_by_slot.contains_key(&Slot::new(10)));
    }

    #[tokio::test]
    async fn search_winner_ticket_rejects_when_difficulty_is_zero() {
        // No ticket can be strictly below zero, so this attempt never wins.
        let result =
            search_winner_ticket(HeaderId::from([7u8; 32]), zero_fr(), Fr::ZERO, test_pool()).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn search_winner_ticket_builds_a_valid_winning_claim() {
        let block_header = HeaderId::from([9u8; 32]);
        let epoch_nonce = zero_fr();
        // Maximum field element: every ticket is below it, so the attempt wins.
        let difficulty = Fr::ZERO - Fr::from(1u64);

        let (secret_key, claim) =
            search_winner_ticket(block_header, epoch_nonce, difficulty, test_pool())
                .await
                .expect("a win is essentially certain at maximum difficulty");

        // The claim reflects the search inputs and the winning key.
        assert_eq!(claim.public_key, secret_key.to_public_key());
        assert_eq!(claim.epoch_nonce, epoch_nonce);
        assert_eq!(claim.block_hash, <[u8; 32]>::from(block_header));
        // The winning ticket genuinely satisfies the difficulty target.
        assert!(
            claim
                .get_puzzle_ticket()
                .validate_difficulty_reward(&difficulty)
                .is_ok()
        );
    }

    type ProcessedBlockStream =
        Pin<Box<dyn Stream<Item = (EpochState, LedgerState, ProcessedBlockEvent)> + Send>>;

    /// A processed-block stream that never yields (upstream open but idle).
    fn pending_blocks() -> ProcessedBlockStream {
        Box::pin(stream::pending())
    }

    /// A processed-block stream that is already closed (upstream terminated).
    fn no_blocks() -> ProcessedBlockStream {
        Box::pin(stream::empty())
    }

    /// A per-block search stream that yields a single winner, then ends.
    fn ready_search(
        block_slot: Slot,
        ticket: (UnsecuredZkKey, ClaimPowRewardOp),
    ) -> WinnerTicketStream {
        Box::pin(stream::iter([(block_slot, ticket)]))
    }

    /// A winning ticket owned by its own freshly generated key.
    fn ticket() -> (UnsecuredZkKey, ClaimPowRewardOp) {
        let secret_key = UnsecuredZkKey::from_rng(&mut rand::thread_rng());
        let claim = ClaimPowRewardOp {
            epoch_nonce: zero_fr(),
            block_hash: [0u8; 32],
            public_key: secret_key.to_public_key(),
        };
        (secret_key, claim)
    }

    /// Polls the generator once with a no-op waker.
    fn poll_once(generator: &mut TicketGenerator) -> Poll<Option<WinningTicket>> {
        let mut cx = Context::from_waker(noop_waker_ref());
        Pin::new(generator).poll_next(&mut cx)
    }

    #[tokio::test]
    async fn poll_terminates_when_upstream_is_closed() {
        // A closed upstream means no block will ever arrive again, so the
        // generator stops mining and ends.
        let mut generator = TicketGenerator {
            processed_block_stream: no_blocks(),
            tickets_search: StreamMap::new(),
            tickets_search_by_slot: HashMap::new(),
            tip: HeaderId::from([0u8; 32]),
            pool: test_pool(),
            max_tickets_per_block: NonZeroUsize::new(4).unwrap(),
            slot_window: SLOT_WINDOW,
        };
        assert!(matches!(poll_once(&mut generator), Poll::Ready(None)));
    }

    #[tokio::test]
    async fn poll_terminates_on_closed_upstream_dropping_active_searches() {
        // Even with a search still running, a closed upstream terminates the
        // generator: we want to stop mining, not keep churning on old blocks.
        let mut tickets_search = StreamMap::new();
        tickets_search.insert(
            HeaderId::from([1u8; 32]),
            Box::pin(stream::pending()) as WinnerTicketStream,
        );
        let mut generator = TicketGenerator {
            processed_block_stream: no_blocks(),
            tickets_search,
            tickets_search_by_slot: HashMap::new(),
            tip: HeaderId::from([0u8; 32]),
            pool: test_pool(),
            max_tickets_per_block: NonZeroUsize::new(4).unwrap(),
            slot_window: SLOT_WINDOW,
        };
        assert!(matches!(poll_once(&mut generator), Poll::Ready(None)));
    }

    #[tokio::test]
    async fn poll_stays_pending_while_upstream_is_open_but_idle() {
        // Open (pending) upstream and no active searches: nothing to emit, but
        // the generator must not terminate.
        let mut generator = TicketGenerator {
            processed_block_stream: pending_blocks(),
            tickets_search: StreamMap::new(),
            tickets_search_by_slot: HashMap::new(),
            tip: HeaderId::from([0u8; 32]),
            pool: test_pool(),
            max_tickets_per_block: NonZeroUsize::new(16).unwrap(),
            slot_window: SLOT_WINDOW,
        };
        assert!(matches!(poll_once(&mut generator), Poll::Pending));
    }

    #[tokio::test]
    async fn poll_emits_ready_winner_tagged_with_cached_tip() {
        let tip = HeaderId::from([42u8; 32]);
        let block_slot = Slot::new(7);
        let (secret_key, claim) = ticket();

        let mut tickets_search = StreamMap::new();
        tickets_search.insert(
            HeaderId::from([1u8; 32]),
            ready_search(block_slot, (secret_key.clone(), claim)),
        );
        let mut generator = TicketGenerator {
            processed_block_stream: pending_blocks(),
            tickets_search,
            tickets_search_by_slot: HashMap::new(),
            tip,
            pool: test_pool(),
            max_tickets_per_block: NonZeroUsize::new(16).unwrap(),
            slot_window: SLOT_WINDOW,
        };

        let Poll::Ready(Some(winner)) = poll_once(&mut generator) else {
            panic!("expected a winning ticket");
        };
        assert_eq!(winner.tip, tip);
        assert_eq!(winner.block_slot, block_slot);
        assert_eq!(winner.claim.public_key, secret_key.to_public_key());
    }

    #[tokio::test]
    async fn poll_drains_ready_winner_before_terminating() {
        let mut tickets_search = StreamMap::new();
        tickets_search.insert(
            HeaderId::from([1u8; 32]),
            ready_search(Slot::new(3), ticket()),
        );
        let mut generator = TicketGenerator {
            processed_block_stream: no_blocks(),
            tickets_search,
            tickets_search_by_slot: HashMap::new(),
            tip: HeaderId::from([0u8; 32]),
            pool: test_pool(),
            max_tickets_per_block: NonZeroUsize::new(4).unwrap(),
            slot_window: SLOT_WINDOW,
        };

        // A winner already produced is emitted first...
        assert!(matches!(poll_once(&mut generator), Poll::Ready(Some(_))));
        // ...then, with the search drained and upstream closed, it terminates.
        assert!(matches!(poll_once(&mut generator), Poll::Ready(None)));
    }
}
