//! Proof-of-Work reward ticket generation.
//!
//! Watches the chain for newly processed blocks and, for every block still
//! within the reward window, runs a concurrent search for a "winning" ticket:
//! a random key whose derived puzzle ticket meets the block's difficulty
//! target. Winning tickets are surfaced through the [`TicketGenerator`] stream.

use std::{
    collections::{HashMap, HashSet},
    iter,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt as _, future::BoxFuture, stream};
use lb_chain_service::{
    ConsensusMsg, EpochState, ProcessedBlockEvent, Slot, api::CryptarchiaServiceApi,
};
use lb_core::{
    crypto::ZkHash,
    header::HeaderId,
    mantle::ops::pow::{ClaimPowRewardOp, PowTarget, SLOT_WINDOW},
};
use lb_key_management_system_keys::keys::UnsecuredZkKey;
use lb_ledger::LedgerState;
use tokio_stream::{
    StreamMap,
    wrappers::{BroadcastStream, errors::BroadcastStreamRecvError},
};
use tracing::{error, log::warn};

/// A winning Proof-of-Work reward: the secret key that produced the ticket,
/// paired with the reward-claim operation to be published.
pub type WinnerTicket = (UnsecuredZkKey, ClaimPowRewardOp);
/// A boxed stream of [`WinnerTicket`]s found for a single block, each tagged
/// with that block's slot (for reward-window bookkeeping).
pub type WinnerTicketStream = Pin<Box<dyn Stream<Item = (Slot, WinnerTicket)> + Send>>;
/// A boxed future resolving to a single reward-claim operation.
pub type TicketSearchTask<'a> = BoxFuture<'a, ClaimPowRewardOp>;

/// A winning ticket together with the chain tip observed when it was found.
///
/// The generator caches the tip from its block-event stream, so consumers can
/// size the reward-claim transaction against the current state without an extra
/// round-trip to the chain service.
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
pub struct TicketGenerator<Tx, CryptarchiaServiceData, RuntimeServiceId>
where
    CryptarchiaServiceData:
        Send + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
{
    /// Handle to the chain service, used to subscribe to processed blocks and
    /// to query the epoch and ledger state of each block.
    cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
    /// Stream of processed blocks, each enriched with the epoch and ledger
    /// state required to search for tickets.
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
}

impl<Tx, CryptarchiaServiceData, RuntimeServiceId>
    TicketGenerator<Tx, CryptarchiaServiceData, RuntimeServiceId>
where
    CryptarchiaServiceData:
        Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
    RuntimeServiceId: Send + Sync + 'static,
    Tx: Send + Sync + 'static,
{
    /// Creates a new [`TicketGenerator`].
    ///
    /// Subscribes to the chain service's new-block stream and wires the
    /// enrichment pipeline that attaches each block's epoch and ledger state.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription to the chain service fails.
    pub async fn new(
        cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
    ) -> Result<Self, lb_chain_service::api::ApiError> {
        let stream = BroadcastStream::new(cryptarchia_api.subscribe_new_blocks().await?);
        let cryptarchia_api_send = cryptarchia_api.clone();
        let processed_block_stream: Pin<
            Box<dyn Stream<Item = (EpochState, LedgerState, ProcessedBlockEvent)> + Send>,
        > = Box::pin(stream.filter_map(move |event| {
            let cryptarchia_api = cryptarchia_api_send.clone();
            async move { Self::process_block_event(event, cryptarchia_api).await }
        }));
        Ok(Self {
            cryptarchia_api,
            processed_block_stream,
            tickets_search: StreamMap::new(),
            tickets_search_by_slot: HashMap::new(),
            tip: HeaderId::from([0u8; 32]),
        })
    }

    /// Enriches a raw processed-block event with its epoch and ledger state.
    ///
    /// Returns `None` (dropping the event) when the broadcast subscription
    /// lagged, or when the epoch or ledger state for the block cannot be
    /// fetched from the chain service.
    async fn process_block_event(
        event: Result<ProcessedBlockEvent, BroadcastStreamRecvError>,
        cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
    ) -> Option<(EpochState, LedgerState, ProcessedBlockEvent)> {
        match event {
            Ok(
                event @ ProcessedBlockEvent {
                    block_id, tip_slot, ..
                },
            ) => {
                let Ok(Ok(epoch_state)) = cryptarchia_api.get_epoch_state(tip_slot).await else {
                    warn!("Epoch state not found for block slot: {tip_slot:?}");
                    return None;
                };
                let Ok(Some(ledger_state)) = cryptarchia_api.get_ledger_state(block_id).await
                else {
                    warn!("Ledger state not found for block: {block_id:?}");
                    return None;
                };
                Some((epoch_state, ledger_state, event))
            }
            Err(e) => {
                error!("Missed new block event due to: {e}");
                None
            }
        }
    }

    /// Builds an unbounded stream that searches for winning tickets for a
    /// single block.
    ///
    /// Up to 16 attempts run concurrently; each draws a fresh random key and
    /// checks the resulting ticket against the block's difficulty target. The
    /// stream yields every winning `(secret key, claim)` pair it finds and
    /// never terminates on its own — it is dropped once the block leaves the
    /// reward window (see [`Self::prune_out_of_window_streams`]).
    fn new_block_search_stream(
        block_header: HeaderId,
        block_slot: Slot,
        epoch_state: &EpochState,
        ledger_state: &LedgerState,
    ) -> WinnerTicketStream
    where
        CryptarchiaServiceData:
            'static + Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>>,
        RuntimeServiceId: 'static + Send + Sync + Unpin,
        Tx: 'static + Send + Sync,
    {
        let epoch_nonce = epoch_state.nonce;
        let difficulty = ledger_state.mantle_ledger().pow.reward_difficulty();
        #[expect(
            rustc::closure_returning_async_block,
            reason = "`repeat_with` takes a FnMut not an async closure"
        )]
        let tasks =
            iter::repeat_with(move || search_winner_ticket(block_header, epoch_nonce, difficulty));
        let results = stream::iter(tasks).buffer_unordered(16);
        let winners = tokio_stream::StreamExt::filter_map(results, |maybe_winner| maybe_winner);
        // Tag every winner with the block's slot so the consumer can track the
        // reward window.
        Box::pin(winners.map(move |ticket| (block_slot, ticket)))
    }
}

/// Runs a single ticket-search attempt for a block.
///
/// Generates a random key, builds the reward claim, and validates its puzzle
/// ticket against `difficulty`. The heavy computation is off-loaded to a
/// blocking thread so it does not stall the async runtime. Returns the winning
/// `(key, claim)` when the ticket meets the difficulty target, otherwise
/// `None`.
async fn search_winner_ticket(
    block_header: HeaderId,
    epoch_nonce: ZkHash,
    difficulty: PowTarget,
) -> Option<(UnsecuredZkKey, ClaimPowRewardOp)> {
    // Ticket computation is heavy, need to be run in blocking threads not to block
    // async execution.
    let task = tokio::task::spawn_blocking(move || {
        let mut rng = rand::thread_rng();
        let sk = UnsecuredZkKey::from_rng(&mut rng);
        let pk = sk.to_public_key();
        let claim = ClaimPowRewardOp {
            epoch_nonce,
            block_hash: block_header.into(),
            public_key: pk,
        };
        let ticket = claim.get_puzzle_ticket();
        ticket
            .validate_difficulty_reward(&difficulty)
            .is_ok()
            .then_some((sk, claim))
    });
    task.await.ok().flatten()
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

impl<Tx, CryptarchiaServiceData, RuntimeServiceId> Stream
    for TicketGenerator<Tx, CryptarchiaServiceData, RuntimeServiceId>
where
    CryptarchiaServiceData:
        Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
    RuntimeServiceId: Send + Sync + Unpin + 'static,
    Tx: Send + Sync + 'static,
{
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

        if let Poll::Ready(Some((
            ref epoch_state,
            ref ledger_state,
            ProcessedBlockEvent {
                block_id,
                block_slot,
                tip,
                tip_slot,
                ..
            },
        ))) = this.processed_block_stream.poll_next_unpin(cx)
        {
            this.tip = tip;
            // compute which slot is old enough
            let frontier_slot = tip_slot.saturating_sub(Slot::new(SLOT_WINDOW));
            // trigger new stream if its new enough
            if frontier_slot < block_slot {
                let stream =
                    Self::new_block_search_stream(block_id, block_slot, epoch_state, ledger_state);
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
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use futures::stream;
    use lb_chain_service::Slot;
    use lb_core::header::HeaderId;
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use tokio_stream::StreamMap;

    use super::{WinnerTicketStream, prune_out_of_window_streams, search_winner_ticket};

    /// A never-resolving search stream, used to populate the map under test.
    fn pending_stream() -> WinnerTicketStream {
        Box::pin(stream::pending())
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
        let result = search_winner_ticket(HeaderId::from([7u8; 32]), zero_fr(), Fr::ZERO).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn search_winner_ticket_builds_a_valid_winning_claim() {
        let block_header = HeaderId::from([9u8; 32]);
        let epoch_nonce = zero_fr();
        // Maximum field element: every ticket is below it, so the attempt wins.
        let difficulty = Fr::ZERO - Fr::from(1u64);

        let (secret_key, claim) = search_winner_ticket(block_header, epoch_nonce, difficulty)
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
}
